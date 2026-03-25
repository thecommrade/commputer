//! RPC client for communicating with the running commputer node.

use serde::{Deserialize, Serialize};

/// Connection to the local node's RPC server.
pub struct NodeClient {
    base_url: String,
    client: reqwest::Client,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChainStatus {
    pub height: u64,
    pub total_supply: u64,
    pub emitted: u64,
    pub burned: u64,
    pub circulating: u64,
    pub remaining: u64,
    pub accounts: usize,
    pub epoch: u64,
    pub pending_txs: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BalanceInfo {
    pub address: String,
    pub balance: u64,
    pub tier: String,
    pub nonce: u64,
    pub is_validator: bool,
    pub total_mined: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub ip: Option<String>,
    pub validator_address: Option<String>,
    pub compliance_status: Option<String>,
}

impl NodeClient {
    /// Create a new client connecting to the node RPC on the given port.
    pub fn new(rpc_port: u16) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", rpc_port),
            client: reqwest::Client::new(),
        }
    }

    /// Get chain status from the node.
    pub async fn status(&self) -> Result<ChainStatus, String> {
        let url = format!("{}/status", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {}", e))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse status: {}", e))
    }

    /// Get balance for an address.
    pub async fn balance(&self, address: &str) -> Result<BalanceInfo, String> {
        let url = format!("{}/balance/{}", self.base_url, address);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {}", e))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse balance: {}", e))
    }

    /// Get connected peers.
    pub async fn peers(&self) -> Result<Vec<PeerInfo>, String> {
        let url = format!("{}/peers", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {}", e))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse peers: {}", e))
    }

    /// Check if the node is reachable.
    pub async fn health(&self) -> bool {
        let url = format!("{}/health", self.base_url);
        self.client.get(&url).send().await.is_ok()
    }

    /// Get node metrics.
    pub async fn metrics(&self) -> Result<serde_json::Value, String> {
        let url = format!("{}/metrics", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {}", e))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse metrics: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_creation() {
        let client = NodeClient::new(9944);
        assert_eq!(client.base_url, "http://127.0.0.1:9944");
    }
}
