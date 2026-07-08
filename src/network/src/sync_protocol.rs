//! Dedicated sync protocol using libp2p request-response.
//! Separate from gossipsub — no rate limiting, direct peer-to-peer.
//! Used for initial block download and catching up after disconnection.

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::request_response;
use libp2p::StreamProtocol;

/// Protocol identifier for the sync protocol.
pub const SYNC_PROTOCOL: StreamProtocol = StreamProtocol::new("/commputer/sync/1");

/// Hard cap on a decoded sync message. Matches the previous inline limit so the
/// legitimate maximum sync message (a full block range) keeps working.
const MAX_SYNC_MESSAGE: usize = 10 * 1024 * 1024;

/// Upper bound on a single read and on the up-front allocation. The declared
/// length in the 4-byte header no longer dictates the allocation: the message is
/// assembled incrementally in chunks of at most this size, so a tiny header can
/// never force a large victim allocation before any payload has arrived
/// (F29 sibling of the consensus_protocol fix).
const READ_CHUNK: usize = 64 * 1024;

/// Read a big-endian u32 length prefix followed by that many bytes, WITHOUT
/// pre-allocating the attacker-declared length.
///
/// The declared length is first rejected if it exceeds `max`. The payload is
/// then read in bounded `READ_CHUNK` slices and appended, so the buffer grows
/// only as bytes actually arrive. A peer that declares the maximum length but
/// stalls after the header commits at most `READ_CHUNK`-sized allocations rather
/// than the full declared size.
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

/// A sync request — ask a peer for a block at a specific height.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SyncRequest {
    /// Request a single block by height.
    GetBlock { height: u64 },
    /// Request a range of blocks (inclusive).
    GetBlocks { start: u64, end: u64 },
    /// Request the peer's current chain height.
    GetHeight,
}

/// A sync response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SyncResponse {
    /// A single block (or None if the peer doesn't have it).
    Block(Option<Vec<u8>>), // Serialized Block bytes
    /// Multiple blocks.
    Blocks(Vec<Vec<u8>>), // Each is a serialized Block
    /// The peer's current chain height.
    Height(u64),
}

/// Codec for the sync protocol — serializes/deserializes with JSON + length prefix.
#[derive(Debug, Clone, Default)]
pub struct SyncCodec;

#[async_trait]
impl request_response::Codec for SyncCodec {
    type Protocol = StreamProtocol;
    type Request = SyncRequest;
    type Response = SyncResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let buf = read_length_prefixed(io, MAX_SYNC_MESSAGE).await?;
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
        let buf = read_length_prefixed(io, MAX_SYNC_MESSAGE).await?;
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

/// Create the request-response behaviour for the sync protocol.
/// High capacity for initial block download — nodes may request hundreds of blocks.
pub fn sync_behaviour() -> request_response::Behaviour<SyncCodec> {
    let config = request_response::Config::default()
        .with_max_concurrent_streams(8);
    request_response::Behaviour::new(
        [(SYNC_PROTOCOL, request_response::ProtocolSupport::Full)],
        config,
    )
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

    /// NON-VACUOUS: a legitimate block-range response far larger than one chunk
    /// still decodes correctly, and the reader is never asked for more than
    /// `READ_CHUNK` bytes at once. The pre-fix `vec![0u8; len]` + single
    /// `read_exact` would request the whole ~>300 KiB payload in one go and fail
    /// this bound.
    #[tokio::test]
    async fn read_response_large_message_reads_in_bounded_chunks() {
        let resp = SyncResponse::Blocks(vec![vec![7u8; 300 * 1024]]);
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
        let mut codec = SyncCodec;
        let decoded = codec
            .read_response(&SYNC_PROTOCOL, &mut reader)
            .await
            .expect("valid large response should decode");

        match decoded {
            SyncResponse::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].len(), 300 * 1024);
                assert!(blocks[0].iter().all(|&b| b == 7));
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
    /// requesting a declared-size buffer (pre-fix would `poll_read` a 1 MiB buffer
    /// here).
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
        let mut codec = SyncCodec;
        let result = codec.read_request(&SYNC_PROTOCOL, &mut reader).await;

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
        let declared = (MAX_SYNC_MESSAGE as u32).wrapping_add(1);
        let data = declared.to_be_bytes().to_vec();

        let mut reader = ChunkTrackingReader {
            data,
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = SyncCodec;
        let result = codec.read_response(&SYNC_PROTOCOL, &mut reader).await;

        assert!(result.is_err(), "declared length above the cap must be rejected");
        assert!(
            reader.max_read_len <= 4,
            "oversized declaration must be rejected from the header alone"
        );
    }

    /// Round-trip sanity for a normal request through the incremental reader.
    #[tokio::test]
    async fn read_request_roundtrip() {
        let req = SyncRequest::GetBlocks { start: 100, end: 110 };
        let payload = serde_json::to_vec(&req).unwrap();
        let mut reader = ChunkTrackingReader {
            data: frame(&payload),
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = SyncCodec;
        let decoded = codec
            .read_request(&SYNC_PROTOCOL, &mut reader)
            .await
            .expect("valid request should decode");
        match decoded {
            SyncRequest::GetBlocks { start, end } => {
                assert_eq!(start, 100);
                assert_eq!(end, 110);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
