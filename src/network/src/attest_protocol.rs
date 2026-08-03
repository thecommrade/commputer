//! Peer -> validator attestation protocol (libp2p request-response).
//!
//! One challenge/response per connection: the challenger sends a fresh nonce
//! bound to both peer ids; the responder signs it with its validator wallet key
//! (see `commputer_core::attest`) and returns its public key + signature. The
//! challenger derives and verifies the responder's validator Address. Messages
//! are tiny and fixed-shape; framing mirrors the consensus protocol (a
//! big-endian u32 length prefix + JSON) so it shares the codebase's wire idiom.

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::request_response;
use libp2p::StreamProtocol;

/// Protocol identifier for the attestation handshake.
pub const ATTEST_PROTOCOL: StreamProtocol = StreamProtocol::new("/commputer/attest/1");

/// Attest messages are tiny (a nonce + peer ids, or a pubkey + sig). Cap well
/// above the real max but far below any bulk size — a fixed small ceiling.
const MAX_ATTEST_MESSAGE: usize = 64 * 1024;

/// Read a big-endian u32 length prefix then that many bytes, without
/// pre-allocating the declared length. Mirrors the consensus codec's reader.
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
            "attest message length exceeds maximum",
        ));
    }
    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;
    Ok(buf)
}

/// The challenge a node sends a freshly-connected peer.
///
/// `challenger_peer` / `responder_peer` are `PeerId::to_bytes()`; the responder
/// checks `responder_peer` is its own id and `challenger_peer` is the caller
/// before signing, and both are bound into the signed bytes (anti-relay).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AttestRequest {
    Challenge {
        chain_id: String,
        challenger_peer: Vec<u8>,
        responder_peer: Vec<u8>,
        nonce: [u8; 32],
    },
}

/// The responder's proof of control of a validator key.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AttestResponse {
    /// `pubkey` is 32 bytes, `sig` is 64 bytes (Vec on the wire — serde does not
    /// derive fixed arrays above 32; `commputer_core::attest::verify_attestation`
    /// length-checks both).
    Proof { pubkey: Vec<u8>, sig: Vec<u8> },
    /// The responder declines (e.g. we did not challenge this peer, or the
    /// challenge did not name us). Carries no key material.
    Decline,
}

/// Codec — JSON + length prefix, identical framing to the consensus codec.
#[derive(Debug, Clone, Default)]
pub struct AttestCodec;

#[async_trait]
impl request_response::Codec for AttestCodec {
    type Protocol = StreamProtocol;
    type Request = AttestRequest;
    type Response = AttestResponse;

    async fn read_request<T>(&mut self, _p: &Self::Protocol, io: &mut T) -> std::io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let buf = read_length_prefixed(io, MAX_ATTEST_MESSAGE).await?;
        serde_json::from_slice(&buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn read_response<T>(&mut self, _p: &Self::Protocol, io: &mut T) -> std::io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        let buf = read_length_prefixed(io, MAX_ATTEST_MESSAGE).await?;
        serde_json::from_slice(&buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn write_request<T>(&mut self, _p: &Self::Protocol, io: &mut T, req: Self::Request) -> std::io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let data = serde_json::to_vec(&req).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        io.write_all(&(data.len() as u32).to_be_bytes()).await?;
        io.write_all(&data).await?;
        io.close().await?;
        Ok(())
    }

    async fn write_response<T>(&mut self, _p: &Self::Protocol, io: &mut T, resp: Self::Response) -> std::io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let data = serde_json::to_vec(&resp).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        io.write_all(&(data.len() as u32).to_be_bytes()).await?;
        io.write_all(&data).await?;
        io.close().await?;
        Ok(())
    }
}

/// Create the request-response behaviour for the attestation protocol. One
/// challenge per connection, so concurrency is low.
pub fn attest_behaviour() -> request_response::Behaviour<AttestCodec> {
    let config = request_response::Config::default().with_max_concurrent_streams(4);
    request_response::Behaviour::new(
        [(ATTEST_PROTOCOL, request_response::ProtocolSupport::Full)],
        config,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::request_response::Codec as _;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct MemReader {
        data: Vec<u8>,
        pos: usize,
    }
    impl futures::io::AsyncRead for MemReader {
        fn poll_read(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<std::io::Result<usize>> {
            let this = self.get_mut();
            let n = (this.data.len() - this.pos).min(buf.len());
            buf[..n].copy_from_slice(&this.data[this.pos..this.pos + n]);
            this.pos += n;
            Poll::Ready(Ok(n))
        }
    }
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut m = (payload.len() as u32).to_be_bytes().to_vec();
        m.extend_from_slice(payload);
        m
    }

    #[tokio::test]
    async fn challenge_roundtrips() {
        let req = AttestRequest::Challenge {
            chain_id: "commputer-testnet-3".into(),
            challenger_peer: vec![1, 2, 3],
            responder_peer: vec![4, 5, 6],
            nonce: [9u8; 32],
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let mut r = MemReader { data: frame(&payload), pos: 0 };
        let decoded = AttestCodec.read_request(&ATTEST_PROTOCOL, &mut r).await.unwrap();
        match decoded {
            AttestRequest::Challenge { nonce, responder_peer, .. } => {
                assert_eq!(nonce, [9u8; 32]);
                assert_eq!(responder_peer, vec![4, 5, 6]);
            }
        }
    }

    #[tokio::test]
    async fn proof_roundtrips() {
        let resp = AttestResponse::Proof { pubkey: vec![2u8; 32], sig: vec![3u8; 64] };
        let payload = serde_json::to_vec(&resp).unwrap();
        let mut r = MemReader { data: frame(&payload), pos: 0 };
        match AttestCodec.read_response(&ATTEST_PROTOCOL, &mut r).await.unwrap() {
            AttestResponse::Proof { pubkey, sig } => {
                assert_eq!(pubkey, vec![2u8; 32]);
                assert_eq!(sig, vec![3u8; 64]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversized_declared_length_rejected_from_header() {
        let declared = (MAX_ATTEST_MESSAGE as u32).wrapping_add(1);
        let mut r = MemReader { data: declared.to_be_bytes().to_vec(), pos: 0 };
        assert!(AttestCodec.read_request(&ATTEST_PROTOCOL, &mut r).await.is_err());
    }
}
