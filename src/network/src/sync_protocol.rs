//! Dedicated sync protocol using libp2p request-response.
//! Separate from gossipsub — no rate limiting, direct peer-to-peer.
//! Used for initial block download and catching up after disconnection.

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::request_response;
use libp2p::StreamProtocol;

/// Protocol identifier for the sync protocol.
pub const SYNC_PROTOCOL: StreamProtocol = StreamProtocol::new("/commputer/sync/1");

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

/// Create the request-response behaviour for the sync protocol.
/// High capacity for initial block download — nodes may request hundreds of blocks.
pub fn sync_behaviour() -> request_response::Behaviour<SyncCodec> {
    let config = request_response::Config::default()
        .with_max_concurrent_streams(4);
    request_response::Behaviour::new(
        [(SYNC_PROTOCOL, request_response::ProtocolSupport::Full)],
        config,
    )
}
