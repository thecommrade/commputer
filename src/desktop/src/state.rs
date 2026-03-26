//! Application state for the desktop app.

use serde::{Deserialize, Serialize};

/// Item 23: Resource contribution percentage (1-100), persisted to config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Resource contribution percentage (1-100).
    pub contribution_percent: u8,
    /// RPC port of the running node.
    pub rpc_port: u16,
    /// Auto-start on boot (Item 39).
    pub auto_start: bool,
    /// Show notifications on reward.
    pub notifications: bool,
    /// Theme: "dark", "light", or "system" (Item 31).
    pub theme: String,
    /// Log level for the node.
    pub log_level: String,
    /// Data directory for the node.
    pub data_dir: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            contribution_percent: 100,
            rpc_port: 9944,
            auto_start: false,
            notifications: true,
            theme: "system".to_string(),
            log_level: "info".to_string(),
            data_dir: "./commputer-testnet".to_string(),
        }
    }
}

impl AppConfig {
    /// Load config from the standard location (~/.commputer/desktop.json).
    pub fn load() -> Self {
        let path = config_path();
        if path.exists()
            && let Ok(data) = std::fs::read_to_string(&path)
                && let Ok(config) = serde_json::from_str(&data) {
                    return config;
                }
        Self::default()
    }

    /// Save config to disk.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create config dir: {}", e))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize config: {}", e))?;
        std::fs::write(&path, json)
            .map_err(|e| format!("failed to write config: {}", e))
    }
}

fn config_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".commputer")
        .join("desktop.json")
}

/// Item 32: Node connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    /// Node is synced and producing blocks.
    Synced,
    /// Node is catching up with the network.
    Syncing,
    /// Node is not connected or not responding.
    Disconnected,
}

impl NodeStatus {
    /// CSS color class for the status indicator dot.
    pub fn color(&self) -> &'static str {
        match self {
            NodeStatus::Synced => "green",
            NodeStatus::Syncing => "yellow",
            NodeStatus::Disconnected => "red",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = AppConfig::default();
        assert_eq!(config.contribution_percent, 100);
        assert_eq!(config.theme, "system");
    }

    #[test]
    fn config_roundtrip() {
        let config = AppConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.contribution_percent, config.contribution_percent);
        assert_eq!(parsed.theme, config.theme);
    }

    #[test]
    fn node_status_colors() {
        assert_eq!(NodeStatus::Synced.color(), "green");
        assert_eq!(NodeStatus::Syncing.color(), "yellow");
        assert_eq!(NodeStatus::Disconnected.color(), "red");
    }
}
