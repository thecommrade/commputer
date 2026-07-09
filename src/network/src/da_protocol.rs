// da_protocol.rs — libp2p request-response codec for the PoUW data-availability
// fetch protocol (Track-2 Phase 0, DA substrate).
//
// WHAT: a dedicated request-response protocol `/commputer/da/1` over which a node
// pulls a single coded DA chunk (the erasure-coded piece of a job's program‖input
// blob, plus its Merkle inclusion path) from a peer that holds it. Mirrors
// `sync_protocol.rs` EXACTLY in shape: length-prefixed JSON frames, a 10 MiB hard
// cap, and the decompression-bomb-safe incremental `read_length_prefixed` reader
// (a tiny header can never force a large victim allocation before payload arrives).
// The DA outcome is NEVER hashed into consensus (it degrades to Abstain), so this
// layer carries no fork risk; old nodes simply don't negotiate the protocol.
//
// WIRING (INERT until the PROTECTED Phase 2 wire-in): `da_behaviour()` is added as
// a new field on `CommpBehaviour` (network/src/transport.rs — a founder-gated edit
// to that existing non-protected file) and inbound `GetChunk` is served from the
// node-local blob store (node/src/da_store.rs), inbound replies correlated in the
// event_loop swarm match. Nothing here is wired into the running node yet.
// FILES NEEDING CHANGES (later, gated): network/src/transport.rs (CommpBehaviour),
// node/src/event_loop.rs (PROTECTED: `...::Da` swarm arm + pending-fetch map).

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::request_response;
use libp2p::StreamProtocol;
use serde::{Deserialize, Serialize};

/// Protocol identifier for the DA fetch protocol.
pub const DA_PROTOCOL: StreamProtocol = StreamProtocol::new("/commputer/da/1");

/// The protocol name as a `StreamProtocol` (function form, mirrors the const so
/// callers can use either).
pub fn da_protocol() -> StreamProtocol {
    DA_PROTOCOL
}

/// Hard cap on a decoded DA message. Matches `sync_protocol.rs`'s MAX_SYNC_MESSAGE
/// (10 MiB) — a single 64 KiB coded chunk plus its Merkle path is orders of
/// magnitude smaller, so this is a generous DoS backstop, not a working limit.
const MAX_DA_MESSAGE: usize = 10 * 1024 * 1024;

/// Upper bound on a single read and on the up-front allocation. Identical to
/// `sync_protocol.rs`: the declared length in the 4-byte header never dictates the
/// allocation; the message is assembled incrementally in chunks of at most this
/// size, so a tiny header can never force a large victim allocation before any
/// payload has arrived.
const READ_CHUNK: usize = 64 * 1024;

/// Read a big-endian u32 length prefix followed by that many bytes, WITHOUT
/// pre-allocating the attacker-declared length. Byte-for-byte the same bomb-safe
/// reader as `sync_protocol::read_length_prefixed` (F29 sibling).
async fn read_length_prefixed<T>(io: &mut T, max: usize) -> std::io::Result<Vec<u8>>
where
    T: AsyncRead + Unpin + Send,
{
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message length exceeds maximum",
        ));
    }
    // Do NOT allocate `len` up front. Read in bounded chunks and grow as data
    // arrives; the initial capacity is capped at READ_CHUNK regardless of `len`.
    let chunk_size = len.min(READ_CHUNK);
    let mut chunk = vec![0u8; chunk_size];
    let mut buf: Vec<u8> = Vec::with_capacity(chunk_size);
    let mut remaining = len;
    while remaining > 0 {
        let take = remaining.min(chunk.len());
        io.read_exact(&mut chunk[..take]).await?;
        buf.extend_from_slice(&chunk[..take]);
        remaining -= take;
    }
    Ok(buf)
}

/// A single coded DA chunk as it travels the wire and rests in the blob store: the
/// erasure-coded chunk bytes plus the SERIALIZED Merkle inclusion path (the on-disk
/// `LocalDiskTransport` encoding of a `Vec<Option<[u8;32]>>`). Kept opaque here —
/// the DA facade verifies the path against `da_root`; this codec only ferries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaChunk {
    /// The coded chunk bytes (<= one DA chunk_size, 64 KiB by default).
    pub bytes: Vec<u8>,
    /// The serialized Merkle path proving `bytes` sits at its index under `da_root`.
    pub merkle_path: Vec<u8>,
}

/// A DA request — ask a peer for a single coded chunk by its content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaRequest {
    /// Request one coded chunk by its transport chunk_hash = `sha256(da_root ‖ index_le)`
    /// (position-addressing; the DA facade re-verifies the returned Merkle path against da_root).
    GetChunk { chunk_hash: [u8; 32] },
}

/// A DA response — the requested chunk, or None if the peer doesn't hold it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaResponse {
    /// The requested chunk (or None if this peer is not a provider for it).
    Chunk(Option<DaChunk>),
}

/// Codec for the DA protocol — serializes/deserializes with JSON + length prefix.
#[derive(Debug, Clone, Default)]
pub struct DaCodec;

#[async_trait]
impl request_response::Codec for DaCodec {
    type Protocol = StreamProtocol;
    type Request = DaRequest;
    type Response = DaResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let buf = read_length_prefixed(io, MAX_DA_MESSAGE).await?;
        serde_json::from_slice(&buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        let buf = read_length_prefixed(io, MAX_DA_MESSAGE).await?;
        serde_json::from_slice(&buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> std::io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let data = serde_json::to_vec(&req)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = (data.len() as u32).to_be_bytes();
        io.write_all(&len).await?;
        io.write_all(&data).await?;
        io.close().await?;
        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        resp: Self::Response,
    ) -> std::io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let data = serde_json::to_vec(&resp)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = (data.len() as u32).to_be_bytes();
        io.write_all(&len).await?;
        io.write_all(&data).await?;
        io.close().await?;
        Ok(())
    }
}

/// Create the request-response behaviour for the DA protocol. Mirrors
/// `sync_behaviour()` — a single Full-support protocol with a modest concurrent
/// stream cap (chunk fetches fan out but each is tiny).
pub fn da_behaviour() -> request_response::Behaviour<DaCodec> {
    let config = request_response::Config::default().with_max_concurrent_streams(8);
    request_response::Behaviour::new([(DA_PROTOCOL, request_response::ProtocolSupport::Full)], config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::request_response::Codec as _;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// In-memory `AsyncRead` that records the largest single-read buffer length
    /// requested. The eager pre-fix code (`vec![0u8; len]; read_exact(&mut buf)`)
    /// issues one `poll_read` for the whole declared length, so `max_read_len`
    /// would blow past `READ_CHUNK`; the incremental reader never does.
    struct ChunkTrackingReader {
        data: Vec<u8>,
        pos: usize,
        max_read_len: usize,
    }

    impl futures::io::AsyncRead for ChunkTrackingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            let this = self.get_mut();
            this.max_read_len = this.max_read_len.max(buf.len());
            let remaining = this.data.len() - this.pos;
            let n = remaining.min(buf.len());
            buf[..n].copy_from_slice(&this.data[this.pos..this.pos + n]);
            this.pos += n;
            Poll::Ready(Ok(n))
        }
    }

    /// Prepend a big-endian u32 length prefix, matching the wire format.
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut msg = (payload.len() as u32).to_be_bytes().to_vec();
        msg.extend_from_slice(payload);
        msg
    }

    /// NON-VACUOUS: a full-size coded chunk response (larger than one READ_CHUNK)
    /// decodes correctly, and the reader is never asked for more than `READ_CHUNK`
    /// bytes at once. The pre-fix `vec![0u8; len]` + single `read_exact` would
    /// request the whole payload in one go and fail this bound.
    #[tokio::test]
    async fn read_response_large_chunk_reads_in_bounded_chunks() {
        let resp = DaResponse::Chunk(Some(DaChunk {
            bytes: vec![7u8; 128 * 1024],
            merkle_path: vec![0xabu8; 264],
        }));
        let payload = serde_json::to_vec(&resp).unwrap();
        assert!(
            payload.len() > READ_CHUNK,
            "test payload must exceed one chunk ({} bytes)",
            payload.len()
        );

        let mut reader = ChunkTrackingReader {
            data: frame(&payload),
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = DaCodec;
        let decoded = codec
            .read_response(&DA_PROTOCOL, &mut reader)
            .await
            .expect("valid large response should decode");

        match decoded {
            DaResponse::Chunk(Some(chunk)) => {
                assert_eq!(chunk.bytes.len(), 128 * 1024);
                assert!(chunk.bytes.iter().all(|&b| b == 7));
                assert_eq!(chunk.merkle_path.len(), 264);
                assert!(chunk.merkle_path.iter().all(|&b| b == 0xab));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        assert!(
            reader.max_read_len <= READ_CHUNK,
            "reader was asked for {} bytes in one read; expected <= {}",
            reader.max_read_len,
            READ_CHUNK
        );
    }

    /// NON-VACUOUS: the amplification scenario — a 4-byte header declaring a large
    /// body that is never delivered. `read_request` must error without ever
    /// requesting a declared-size buffer.
    #[tokio::test]
    async fn read_request_truncated_does_not_overallocate() {
        let declared: u32 = 1024 * 1024;
        let mut data = declared.to_be_bytes().to_vec();
        data.extend_from_slice(b"only-a-few-bytes");

        let mut reader = ChunkTrackingReader {
            data,
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = DaCodec;
        let result = codec.read_request(&DA_PROTOCOL, &mut reader).await;

        assert!(result.is_err(), "truncated stream should error");
        assert!(
            reader.max_read_len <= READ_CHUNK,
            "reader was asked for {} bytes for an undelivered body; expected <= {}",
            reader.max_read_len,
            READ_CHUNK
        );
    }

    /// A declared length above the hard cap is rejected from the 4-byte header
    /// alone — no body read is attempted. Keeps the max-message cap working.
    #[tokio::test]
    async fn read_response_rejects_oversized_declared_length() {
        let declared = (MAX_DA_MESSAGE as u32).wrapping_add(1);
        let data = declared.to_be_bytes().to_vec();

        let mut reader = ChunkTrackingReader {
            data,
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = DaCodec;
        let result = codec.read_response(&DA_PROTOCOL, &mut reader).await;

        assert!(result.is_err(), "declared length above the cap must be rejected");
        assert!(
            reader.max_read_len <= 4,
            "oversized declaration must be rejected from the header alone"
        );
    }

    /// Round-trip sanity for a `GetChunk` request through the incremental reader.
    #[tokio::test]
    async fn read_request_roundtrip() {
        let req = DaRequest::GetChunk { chunk_hash: [42u8; 32] };
        let payload = serde_json::to_vec(&req).unwrap();
        let mut reader = ChunkTrackingReader {
            data: frame(&payload),
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = DaCodec;
        let decoded = codec
            .read_request(&DA_PROTOCOL, &mut reader)
            .await
            .expect("valid request should decode");
        match decoded {
            DaRequest::GetChunk { chunk_hash } => assert_eq!(chunk_hash, [42u8; 32]),
        }
    }

    /// Round-trip for the `None` (not-a-provider) response variant.
    #[tokio::test]
    async fn read_response_none_roundtrip() {
        let resp = DaResponse::Chunk(None);
        let payload = serde_json::to_vec(&resp).unwrap();
        let mut reader = ChunkTrackingReader {
            data: frame(&payload),
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = DaCodec;
        let decoded = codec
            .read_response(&DA_PROTOCOL, &mut reader)
            .await
            .expect("valid response should decode");
        assert_eq!(decoded, DaResponse::Chunk(None));
    }

    /// The protocol name string is exactly `/commputer/da/1` and the function and
    /// const forms agree.
    #[test]
    fn protocol_name_is_stable() {
        assert_eq!(da_protocol().as_ref(), "/commputer/da/1");
        assert_eq!(DA_PROTOCOL.as_ref(), "/commputer/da/1");
    }
}
