//! Item 192: Auto-update checker.
//! On startup, checks GitHub releases for a new version.

use serde::{Deserialize, Serialize};

/// Current version of the desktop app (from Cargo.toml).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub releases API response (simplified).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub html_url: String,
    pub published_at: Option<String>,
    pub body: Option<String>,
}

/// Result of an update check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub download_url: Option<String>,
    pub release_notes: Option<String>,
    pub error: Option<String>,
}

/// Update checker that queries GitHub releases.
pub struct UpdateChecker {
    repo_url: String,
    client: reqwest::Client,
}

impl UpdateChecker {
    /// Create a new update checker for the given GitHub repo.
    /// Format: "owner/repo" (e.g. "commputer/commputer").
    pub fn new(repo: &str) -> Self {
        Self {
            repo_url: format!("https://api.github.com/repos/{repo}/releases/latest"),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent("commputer-desktop")
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Check for updates by querying GitHub releases API.
    pub async fn check(&self) -> UpdateCheckResult {
        match self.fetch_latest().await {
            Ok(release) => {
                let latest = release.tag_name.trim_start_matches('v').to_string();
                let update_available = version_is_newer(&latest, CURRENT_VERSION);
                UpdateCheckResult {
                    current_version: CURRENT_VERSION.to_string(),
                    latest_version: Some(latest),
                    update_available,
                    download_url: Some(release.html_url),
                    release_notes: release.body,
                    error: None,
                }
            }
            Err(e) => UpdateCheckResult {
                current_version: CURRENT_VERSION.to_string(),
                latest_version: None,
                update_available: false,
                download_url: None,
                release_notes: None,
                error: Some(e),
            },
        }
    }

    /// Fetch the latest release from GitHub.
    async fn fetch_latest(&self) -> Result<GitHubRelease, String> {
        let resp = self.client.get(&self.repo_url)
            .send().await
            .map_err(|e| format!("Failed to check for updates: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("GitHub API returned status {}", resp.status()));
        }

        resp.json().await
            .map_err(|e| format!("Failed to parse release info: {e}"))
    }

    /// Get the repo URL being checked.
    #[allow(dead_code)]
    pub fn repo_url(&self) -> &str {
        &self.repo_url
    }
}

/// Compare two semver-like version strings.
/// Returns true if `latest` is newer than `current`.
pub fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let latest_parts = parse(latest);
    let current_parts = parse(current);
    for i in 0..latest_parts.len().max(current_parts.len()) {
        let l = latest_parts.get(i).copied().unwrap_or(0);
        let c = current_parts.get(i).copied().unwrap_or(0);
        if l > c {
            return true;
        }
        if l < c {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_newer() {
        assert!(version_is_newer("0.2.0", "0.1.0"));
        assert!(version_is_newer("1.0.0", "0.9.9"));
        assert!(version_is_newer("0.1.1", "0.1.0"));
    }

    #[test]
    fn version_comparison_same() {
        assert!(!version_is_newer("0.1.0", "0.1.0"));
        assert!(!version_is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn version_comparison_older() {
        assert!(!version_is_newer("0.1.0", "0.2.0"));
        assert!(!version_is_newer("0.9.0", "1.0.0"));
    }

    #[test]
    fn version_comparison_different_lengths() {
        assert!(version_is_newer("0.1.1", "0.1"));
        assert!(!version_is_newer("0.1", "0.1.1"));
    }

    #[test]
    fn current_version_is_set() {
        // CURRENT_VERSION comes from Cargo.toml.
        assert!(!CURRENT_VERSION.is_empty());
    }

    #[test]
    fn update_checker_creation() {
        let checker = UpdateChecker::new("commputer/commputer");
        assert!(checker.repo_url().contains("github.com"));
        assert!(checker.repo_url().contains("commputer/commputer"));
    }

    #[test]
    fn version_strips_v_prefix() {
        // The check() method strips 'v' prefix from tag_name.
        let tag = "v0.2.0";
        let stripped = tag.trim_start_matches('v');
        assert_eq!(stripped, "0.2.0");
    }
}
