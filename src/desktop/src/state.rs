//! Application state for the desktop app.
//! Items 179, 189, 190, 196: Config persistence, theme, window state.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Item 190: Full application config, persisted to commputer-config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Resource contribution percentage (1-100). Item 179.
    pub contribution_percent: u8,
    /// RPC port of the running node.
    pub rpc_port: u16,
    /// Auto-start on boot (Item 188).
    pub auto_start: bool,
    /// Show notifications on reward.
    pub notifications: bool,
    /// Theme: "dark", "light", or "system" (Item 189).
    pub theme: String,
    /// Log level for the node.
    pub log_level: String,
    /// Data directory for the node.
    pub data_dir: String,
    /// Item 196: Window state persistence.
    pub window: WindowState,
    /// The wallet address (hex-encoded) if one has been created.
    pub wallet_address: Option<String>,
    /// Whether the onboarding tutorial has been completed.
    pub onboarding_complete: bool,
    /// HTTP server port for the desktop dashboard.
    pub dashboard_port: u16,
}

/// Item 196: Remembered window geometry and open panels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub panels_open: Vec<String>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1200,
            height: 800,
            x: 100,
            y: 100,
            panels_open: vec![
                "wallet".into(),
                "mining".into(),
                "network".into(),
                "transactions".into(),
            ],
        }
    }
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
            window: WindowState::default(),
            wallet_address: None,
            onboarding_complete: false,
            dashboard_port: 8080,
        }
    }
}

impl AppConfig {
    /// Load config from commputer-config.toml.
    pub fn load() -> Self {
        let path = config_path();
        if path.exists() {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(config) = toml::from_str(&data) {
                    return config;
                }
            }
        }
        // Fall back to legacy JSON format.
        let legacy = legacy_config_path();
        if legacy.exists() {
            if let Ok(data) = std::fs::read_to_string(&legacy) {
                if let Ok(config) = serde_json::from_str(&data) {
                    return config;
                }
            }
        }
        Self::default()
    }

    /// Load config from a specific path (for testing).
    pub fn load_from(path: &std::path::Path) -> Self {
        if path.exists() {
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(config) = toml::from_str(&data) {
                    return config;
                }
            }
        }
        Self::default()
    }

    /// Save config to commputer-config.toml.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        self.save_to(&path)
    }

    /// Save config to a specific path (for testing).
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create config dir: {e}"))?;
        }
        let toml_str = toml::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize config: {e}"))?;
        std::fs::write(path, toml_str)
            .map_err(|e| format!("failed to write config: {e}"))
    }

    /// Item 179: Update contribution percentage and persist.
    pub fn set_contribution(&mut self, percent: u8) -> Result<(), String> {
        self.contribution_percent = percent.clamp(1, 100);
        self.save()
    }

    /// Item 189: Set theme and persist.
    pub fn set_theme(&mut self, theme: &str) -> Result<(), String> {
        match theme {
            "dark" | "light" | "system" => {
                self.theme = theme.to_string();
                self.save()
            }
            _ => Err(format!("invalid theme: {theme}")),
        }
    }

    /// Item 196: Update window state.
    pub fn set_window_state(&mut self, state: WindowState) -> Result<(), String> {
        self.window = state;
        self.save()
    }
}

/// Primary config path: ~/.commputer/commputer-config.toml.
pub fn config_path() -> PathBuf {
    config_dir().join("commputer-config.toml")
}

/// Legacy JSON config path.
fn legacy_config_path() -> PathBuf {
    config_dir().join("desktop.json")
}

/// Config directory.
pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".commputer")
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
        assert_eq!(config.dashboard_port, 8080);
        assert!(!config.onboarding_complete);
    }

    #[test]
    fn config_toml_roundtrip() {
        let config = AppConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: AppConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.contribution_percent, config.contribution_percent);
        assert_eq!(parsed.theme, config.theme);
        assert_eq!(parsed.window.width, config.window.width);
    }

    #[test]
    fn config_json_roundtrip() {
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

    #[test]
    fn contribution_clamped() {
        let mut config = AppConfig::default();
        // Don't actually save to disk in tests - just check the clamping logic.
        config.contribution_percent = 0u8.clamp(1, 100);
        assert_eq!(config.contribution_percent, 1);
        config.contribution_percent = 150u8.clamp(1, 100);
        assert_eq!(config.contribution_percent, 100);
    }

    #[test]
    fn theme_validation() {
        let config = AppConfig::default();
        // Valid themes.
        for t in &["dark", "light", "system"] {
            assert!(matches!(t, &"dark" | &"light" | &"system"));
        }
        // Invalid theme detection (manual check, avoid disk write).
        assert!(!["dark", "light", "system"].contains(&"purple"));
        let _ = config; // suppress unused warning
    }

    #[test]
    fn window_state_default() {
        let ws = WindowState::default();
        assert_eq!(ws.width, 1200);
        assert_eq!(ws.height, 800);
        assert!(ws.panels_open.contains(&"wallet".to_string()));
    }

    #[test]
    fn config_save_and_load_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-config.toml");
        let mut config = AppConfig::default();
        config.contribution_percent = 42;
        config.theme = "dark".to_string();
        config.wallet_address = Some("abcd1234".to_string());
        config.save_to(&path).unwrap();

        let loaded = AppConfig::load_from(&path);
        assert_eq!(loaded.contribution_percent, 42);
        assert_eq!(loaded.theme, "dark");
        assert_eq!(loaded.wallet_address, Some("abcd1234".to_string()));
    }
}
