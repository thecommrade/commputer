//! Item 191: Desktop notification system.
//! Stubs for mining reward, tier change, and compliance change notifications.

use serde::{Deserialize, Serialize};

/// Notification categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationType {
    /// Mining reward received.
    MiningReward,
    /// Holder tier changed (up or down).
    TierChange,
    /// Compliance status changed.
    ComplianceChange,
    /// Update available.
    UpdateAvailable,
    /// Generic info notification.
    Info,
}

/// A desktop notification to display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub notification_type: NotificationType,
    pub title: String,
    pub body: String,
    pub timestamp: u64,
}

/// Desktop notification manager.
pub struct Notifier {
    enabled: bool,
    /// History of sent notifications (for the frontend to display).
    history: Vec<Notification>,
    max_history: usize,
}

impl Notifier {
    /// Create a new notifier.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            history: Vec::new(),
            max_history: 100,
        }
    }

    /// Check if notifications are enabled.
    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enable or disable notifications.
    #[allow(dead_code)]
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Get notification history.
    pub fn history(&self) -> &[Notification] {
        &self.history
    }

    /// Clear notification history.
    #[allow(dead_code)]
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Send a notification. Returns true if actually sent (enabled + supported).
    pub fn notify(&mut self, notification_type: NotificationType, title: &str, body: &str) -> bool {
        let notif = Notification {
            notification_type,
            title: title.to_string(),
            body: body.to_string(),
            timestamp: now_secs(),
        };

        // Always add to history regardless of enabled state.
        self.history.push(notif.clone());
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }

        if !self.enabled {
            return false;
        }

        // Platform-specific notification dispatch (stub).
        // In production with Tauri, this would call the native notification API.
        self.dispatch_platform_notification(&notif);
        true
    }

    /// Convenience: notify mining reward.
    #[allow(dead_code)]
    pub fn notify_mining_reward(&mut self, amount_formatted: &str) -> bool {
        self.notify(
            NotificationType::MiningReward,
            "Mining Reward",
            &format!("You received {amount_formatted}"),
        )
    }

    /// Convenience: notify tier change.
    #[allow(dead_code)]
    pub fn notify_tier_change(&mut self, old_tier: &str, new_tier: &str) -> bool {
        let direction = if new_tier > old_tier { "upgraded" } else { "changed" };
        self.notify(
            NotificationType::TierChange,
            "Tier Change",
            &format!("You {direction} from {old_tier} to {new_tier}"),
        )
    }

    /// Convenience: notify compliance change.
    #[allow(dead_code)]
    pub fn notify_compliance_change(&mut self, status: &str) -> bool {
        self.notify(
            NotificationType::ComplianceChange,
            "Compliance Status",
            &format!("Your compliance status is now: {status}"),
        )
    }

    /// Convenience: notify update available.
    pub fn notify_update_available(&mut self, version: &str) -> bool {
        self.notify(
            NotificationType::UpdateAvailable,
            "Update Available",
            &format!("Version {version} is available. Visit GitHub to download."),
        )
    }

    /// Platform-specific notification dispatch (stub).
    fn dispatch_platform_notification(&self, notif: &Notification) {
        // On Linux: would use libnotify / notify-send
        // On macOS: would use NSUserNotification
        // On Windows: would use toast notifications
        tracing::info!(
            "Desktop notification: [{}] {} - {}",
            format!("{:?}", notif.notification_type),
            notif.title,
            notif.body
        );
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifier_creation() {
        let notifier = Notifier::new(true);
        assert!(notifier.is_enabled());
        assert!(notifier.history().is_empty());
    }

    #[test]
    fn notifier_disabled() {
        let mut notifier = Notifier::new(false);
        let sent = notifier.notify(NotificationType::Info, "Test", "Body");
        assert!(!sent);
        // Still recorded in history.
        assert_eq!(notifier.history().len(), 1);
    }

    #[test]
    fn notifier_enabled() {
        let mut notifier = Notifier::new(true);
        let sent = notifier.notify(NotificationType::Info, "Test", "Body");
        assert!(sent);
        assert_eq!(notifier.history().len(), 1);
        assert_eq!(notifier.history()[0].title, "Test");
    }

    #[test]
    fn mining_reward_notification() {
        let mut notifier = Notifier::new(true);
        notifier.notify_mining_reward("10 COMME");
        assert_eq!(notifier.history().len(), 1);
        assert!(notifier.history()[0].body.contains("10 COMME"));
        assert_eq!(notifier.history()[0].notification_type, NotificationType::MiningReward);
    }

    #[test]
    fn tier_change_notification() {
        let mut notifier = Notifier::new(true);
        notifier.notify_tier_change("Base", "Storage");
        assert!(notifier.history()[0].body.contains("Base"));
        assert!(notifier.history()[0].body.contains("Storage"));
    }

    #[test]
    fn compliance_change_notification() {
        let mut notifier = Notifier::new(true);
        notifier.notify_compliance_change("nerfed");
        assert!(notifier.history()[0].body.contains("nerfed"));
    }

    #[test]
    fn update_notification() {
        let mut notifier = Notifier::new(true);
        notifier.notify_update_available("0.2.0");
        assert!(notifier.history()[0].body.contains("0.2.0"));
    }

    #[test]
    fn history_limit() {
        let mut notifier = Notifier::new(true);
        notifier.max_history = 3;
        for i in 0..5 {
            notifier.notify(NotificationType::Info, &format!("Test {i}"), "Body");
        }
        assert_eq!(notifier.history().len(), 3);
        // Oldest should have been removed.
        assert_eq!(notifier.history()[0].title, "Test 2");
    }

    #[test]
    fn clear_history() {
        let mut notifier = Notifier::new(true);
        notifier.notify(NotificationType::Info, "Test", "Body");
        assert_eq!(notifier.history().len(), 1);
        notifier.clear_history();
        assert!(notifier.history().is_empty());
    }

    #[test]
    fn toggle_enabled() {
        let mut notifier = Notifier::new(false);
        assert!(!notifier.is_enabled());
        notifier.set_enabled(true);
        assert!(notifier.is_enabled());
    }
}
