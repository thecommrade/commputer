//! RPC client for communicating with the running commputer node.
//! Items 176, 180, 181, 182, 183, 184, 185: Wire all endpoints.

use serde::{Deserialize, Serialize};

/// Connection to the local node's RPC server.
pub struct NodeClient {
    pub base_url: String,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceInfo {
    pub address: String,
    pub balance: u64,
    pub tier: String,
    pub nonce: u64,
    pub is_validator: bool,
    pub total_mined: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub ip: Option<String>,
    pub validator_address: Option<String>,
    pub compliance_status: Option<String>,
}

/// Item 180: Proof status from /proofs/status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofStatus {
    pub cpu_score: u64,
    pub gpu_score: u64,
    pub storage_score: u64,
    pub ram_score: u64,
    pub bandwidth_score: u64,
    pub total_score: u64,
    pub last_challenge_epoch: u64,
}

/// Item 184: Compliance status from /compliance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceInfo {
    pub status: String,
    pub is_compliant: bool,
    pub explanation: Option<String>,
    pub grace_remaining_secs: Option<u64>,
    pub grace_max_secs: Option<u64>,
}

/// Item 182: Transaction receipt from /receipt/{tx_hash}.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxReceipt {
    pub tx_hash: String,
    pub tx_type: String,
    pub amount: u64,
    pub timestamp: u64,
    pub status: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Item 181: Transaction submission request.
#[derive(Debug, Serialize)]
pub struct TxSubmission {
    pub from: String,
    pub to: String,
    pub amount: u64,
    pub nonce: u64,
}

/// Item 181: Transaction submission response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxSubmissionResult {
    pub tx_hash: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Item 182: Mempool entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MempoolEntry {
    pub tx_hash: String,
    pub tx_type: String,
    pub amount: u64,
    pub from: Option<String>,
    pub to: Option<String>,
    pub timestamp: u64,
}

/// Item 197: Log entry from node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub level: String,
    pub message: String,
    pub timestamp: u64,
    pub target: Option<String>,
}

/// Item 198: Network quality info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkQuality {
    pub latency_ms: u64,
    pub peers_connected: usize,
    pub bandwidth_in_kbps: u64,
    pub bandwidth_out_kbps: u64,
}

/// Block info from /block/{height}.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockInfo {
    pub height: u64,
    pub hash: String,
    pub timestamp: u64,
    pub tx_count: usize,
    pub producer: String,
}

impl NodeClient {
    /// Create a new client connecting to the node RPC on the given port.
    pub fn new(rpc_port: u16) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", rpc_port),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Item 176: Get chain status from the node.
    pub async fn status(&self) -> Result<ChainStatus, String> {
        let url = format!("{}/status", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse status: {e}"))
    }

    /// Item 176: Get balance for an address.
    pub async fn balance(&self, address: &str) -> Result<BalanceInfo, String> {
        let url = format!("{}/balance/{}", self.base_url, address);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse balance: {e}"))
    }

    /// Item 183: Get connected peers.
    pub async fn peers(&self) -> Result<Vec<PeerInfo>, String> {
        let url = format!("{}/peers", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse peers: {e}"))
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
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse metrics: {e}"))
    }

    /// Item 180: Get proof status.
    pub async fn proof_status(&self) -> Result<ProofStatus, String> {
        let url = format!("{}/proofs/status", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse proof status: {e}"))
    }

    /// Item 184: Get compliance status.
    pub async fn compliance(&self) -> Result<ComplianceInfo, String> {
        let url = format!("{}/compliance", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse compliance: {e}"))
    }

    /// Item 181: Submit a transaction.
    pub async fn submit_tx(&self, tx: &TxSubmission) -> Result<TxSubmissionResult, String> {
        let url = format!("{}/tx", self.base_url);
        let resp = self.client.post(&url)
            .json(tx)
            .send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse tx result: {e}"))
    }

    /// Item 182: Get mempool entries.
    pub async fn mempool(&self) -> Result<Vec<MempoolEntry>, String> {
        let url = format!("{}/mempool", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse mempool: {e}"))
    }

    /// Item 182: Get transaction receipt.
    pub async fn receipt(&self, tx_hash: &str) -> Result<TxReceipt, String> {
        let url = format!("{}/receipt/{}", self.base_url, tx_hash);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse receipt: {e}"))
    }

    /// Get block by height.
    pub async fn block(&self, height: u64) -> Result<BlockInfo, String> {
        let url = format!("{}/block/{}", self.base_url, height);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse block: {e}"))
    }

    /// Item 198: Get network quality metrics.
    pub async fn network_quality(&self) -> Result<NetworkQuality, String> {
        let url = format!("{}/network/quality", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| format!("RPC request failed: {e}"))?;
        resp.json().await
            .map_err(|e| format!("Failed to parse network quality: {e}"))
    }
}

/// Item 198: Render a text-based network visualization of peers.
pub fn render_peer_map(peers: &[PeerInfo]) -> String {
    if peers.is_empty() {
        return "  [No peers connected]\n".to_string();
    }
    let mut out = String::new();
    out.push_str("  [YOU] -- Local Node\n");
    for (i, peer) in peers.iter().enumerate() {
        let status_char = match peer.compliance_status.as_deref() {
            Some("compliant") => '+',
            Some("nerfed") => '!',
            _ => '?',
        };
        let addr = peer.validator_address.as_deref().unwrap_or("unknown");
        let ip = peer.ip.as_deref().unwrap_or("?.?.?.?");
        let branch = if i == peers.len() - 1 { "\\--" } else { "|--" };
        out.push_str(&format!(
            "   {branch} [{status_char}] {short_id} ({ip}) addr={short_addr}\n",
            short_id = &peer.peer_id[..peer.peer_id.len().min(12)],
            short_addr = &addr[..addr.len().min(16)],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_creation() {
        let client = NodeClient::new(9944);
        assert_eq!(client.base_url, "http://127.0.0.1:9944");
    }

    #[test]
    fn client_custom_port() {
        let client = NodeClient::new(8080);
        assert_eq!(client.base_url, "http://127.0.0.1:8080");
    }

    #[test]
    fn peer_map_empty() {
        let map = render_peer_map(&[]);
        assert!(map.contains("No peers connected"));
    }

    #[test]
    fn peer_map_with_peers() {
        let peers = vec![
            PeerInfo {
                peer_id: "12D3KooWAbcdef".to_string(),
                ip: Some("192.168.1.1".to_string()),
                validator_address: Some("abcdef1234567890".to_string()),
                compliance_status: Some("compliant".to_string()),
            },
            PeerInfo {
                peer_id: "12D3KooWXyzabc".to_string(),
                ip: None,
                validator_address: None,
                compliance_status: Some("nerfed".to_string()),
            },
        ];
        let map = render_peer_map(&peers);
        assert!(map.contains("[YOU]"));
        assert!(map.contains("[+]"));
        assert!(map.contains("[!]"));
        assert!(map.contains("192.168.1.1"));
    }

    #[test]
    fn tx_submission_serializes() {
        let tx = TxSubmission {
            from: "abc".to_string(),
            to: "def".to_string(),
            amount: 1000,
            nonce: 1,
        };
        let json = serde_json::to_string(&tx).unwrap();
        assert!(json.contains("\"from\":\"abc\""));
    }
}
