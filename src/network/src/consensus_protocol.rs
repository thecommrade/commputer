//! Dedicated consensus protocol using libp2p request-response.
//! Separate from gossipsub — no rate limiting, direct peer-to-peer.
//! Used for block proposals and votes during consensus rounds.

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::request_response;
use libp2p::StreamProtocol;

/// Protocol identifier for the consensus protocol.
pub const CONSENSUS_PROTOCOL: StreamProtocol = StreamProtocol::new("/commputer/consensus/1");

/// Hard cap on a decoded consensus message. Matches the previous limit so the
/// legitimate maximum message size keeps working (a full BlockProposal fits).
const MAX_CONSENSUS_MESSAGE: usize = 10 * 1024 * 1024;

/// Upper bound on a single read and on the up-front allocation. The declared
/// length in the 4-byte header no longer dictates the allocation: the message is
/// assembled incrementally in chunks of at most this size, so a tiny header can
/// never force a large victim allocation before any payload has arrived.
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

/// A consensus request — leader sends proposals or requests votes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ConsensusRequest {
    /// Leader sends full block proposal.
    BlockProposal { block_bytes: Vec<u8>, height: u64 },
    /// Leader requests a vote from a peer that hasn't responded.
    VoteRequest { height: u64, block_hash: [u8; 32] },
}

/// A consensus response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ConsensusResponse {
    /// Peer validates and votes.
    Vote { height: u64, preference: [u8; 32], accept: bool },
    /// Peer is not ready: it cannot endorse this height right now.
    ///
    /// `tip` is the responder's applied height, so the asker can tell a peer
    /// that is genuinely BEHIND (and will catch up) from one that is level or
    /// ahead (and is refusing, or forked). Without it, an unbounded "peer is
    /// syncing, be patient" reset let a permanently-wedged node suppress its
    /// neighbours' stall recovery forever.
    ///
    /// Additive and `#[serde(default)]`: pre-alpha.6 peers send no `tip` and
    /// decode as 0 ("unknown"), and they ignore the field we send, so mixed
    /// versions interoperate.
    NotReady {
        height: u64,
        #[serde(default)]
        tip: u64,
    },
}

/// Codec for the consensus protocol — serializes/deserializes with JSON + length prefix.
#[derive(Debug, Clone, Default)]
pub struct ConsensusCodec;

#[async_trait]
impl request_response::Codec for ConsensusCodec {
    type Protocol = StreamProtocol;
    type Request = ConsensusRequest;
    type Response = ConsensusResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let buf = read_length_prefixed(io, MAX_CONSENSUS_MESSAGE).await?;
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
        let buf = read_length_prefixed(io, MAX_CONSENSUS_MESSAGE).await?;
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

/// Create the request-response behaviour for the consensus protocol.
/// Limited concurrency — consensus rounds are sequential, not bulk download.
pub fn consensus_behaviour() -> request_response::Behaviour<ConsensusCodec> {
    let config = request_response::Config::default()
        .with_max_concurrent_streams(4);
    request_response::Behaviour::new(
        [(CONSENSUS_PROTOCOL, request_response::ProtocolSupport::Full)],
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

    /// NON-VACUOUS: a legitimate message far larger than one chunk still decodes
    /// correctly, and the reader is never asked for more than `READ_CHUNK` bytes
    /// at once. The pre-fix `vec![0u8; len]` + single `read_exact` would request
    /// the whole ~>300 KiB payload in one go and fail this bound.
    #[tokio::test]
    async fn read_request_large_message_reads_in_bounded_chunks() {
        let req = ConsensusRequest::BlockProposal {
            block_bytes: vec![7u8; 300 * 1024],
            height: 42,
        };
        let payload = serde_json::to_vec(&req).unwrap();
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
        let mut codec = ConsensusCodec;
        let decoded = codec
            .read_request(&CONSENSUS_PROTOCOL, &mut reader)
            .await
            .expect("valid large message should decode");

        match decoded {
            ConsensusRequest::BlockProposal { block_bytes, height } => {
                assert_eq!(height, 42);
                assert_eq!(block_bytes.len(), 300 * 1024);
                assert!(block_bytes.iter().all(|&b| b == 7));
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
    /// body that is never delivered. Must error without ever requesting a
    /// declared-size buffer (pre-fix would `poll_read` a 1 MiB buffer here).
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
        let mut codec = ConsensusCodec;
        let result = codec.read_request(&CONSENSUS_PROTOCOL, &mut reader).await;

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
    async fn read_request_rejects_oversized_declared_length() {
        let declared = (MAX_CONSENSUS_MESSAGE as u32).wrapping_add(1);
        let data = declared.to_be_bytes().to_vec();

        let mut reader = ChunkTrackingReader {
            data,
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = ConsensusCodec;
        let result = codec.read_request(&CONSENSUS_PROTOCOL, &mut reader).await;

        assert!(result.is_err(), "declared length above the cap must be rejected");
        assert!(
            reader.max_read_len <= 4,
            "oversized declaration must be rejected from the header alone"
        );
    }

    /// The maximum legitimate message size (exactly the cap) is still accepted.
    #[tokio::test]
    async fn read_request_accepts_message_at_cap() {
        // A block_bytes vector whose JSON stays under the cap but whose declared
        // frame length is well above a chunk — exercises the full assembly path.
        let req = ConsensusRequest::BlockProposal {
            block_bytes: vec![1u8; 512 * 1024],
            height: 7,
        };
        let payload = serde_json::to_vec(&req).unwrap();
        assert!(payload.len() <= MAX_CONSENSUS_MESSAGE);

        let mut reader = ChunkTrackingReader {
            data: frame(&payload),
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = ConsensusCodec;
        let decoded = codec
            .read_request(&CONSENSUS_PROTOCOL, &mut reader)
            .await
            .expect("message within the cap must decode");
        match decoded {
            ConsensusRequest::BlockProposal { block_bytes, height } => {
                assert_eq!(height, 7);
                assert_eq!(block_bytes.len(), 512 * 1024);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    /// `read_response` shares the same helper — confirm it round-trips.
    #[tokio::test]
    async fn read_response_roundtrip() {
        let resp = ConsensusResponse::Vote {
            height: 9,
            preference: [3u8; 32],
            accept: true,
        };
        let payload = serde_json::to_vec(&resp).unwrap();
        let mut reader = ChunkTrackingReader {
            data: frame(&payload),
            pos: 0,
            max_read_len: 0,
        };
        let mut codec = ConsensusCodec;
        let decoded = codec
            .read_response(&CONSENSUS_PROTOCOL, &mut reader)
            .await
            .expect("valid response should decode");
        match decoded {
            ConsensusResponse::Vote { height, preference, accept } => {
                assert_eq!(height, 9);
                assert_eq!(preference, [3u8; 32]);
                assert!(accept);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
