//! Dedicated consensus protocol using libp2p request-response.
//! Separate from gossipsub — no rate limiting, direct peer-to-peer.
//! Used for block proposals and votes during consensus rounds.

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::request_response;
use libp2p::StreamProtocol;

/// Protocol identifier for the consensus protocol.
pub const CONSENSUS_PROTOCOL: StreamProtocol = StreamProtocol::new("/commputer/consensus/1");

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
    /// Peer is not ready (still syncing).
    NotReady { height: u64 },
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
        let mut len_buf = [0u8; 4];
        io.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > 10 * 1024 * 1024 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "request too large"));
        }
        let mut buf = vec![0u8; len];
        io.read_exact(&mut buf).await?;
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
        let mut len_buf = [0u8; 4];
        io.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > 10 * 1024 * 1024 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "response too large"));
        }
        let mut buf = vec![0u8; len];
        io.read_exact(&mut buf).await?;
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
