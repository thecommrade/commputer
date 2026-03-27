//! Item 187: System tray icon concept.
//! Platform-specific stubs for tray icon management.

use serde::{Deserialize, Serialize};

/// Tray icon status reflecting node state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayIconState {
    /// Node synced and mining.
    Active,
    /// Node syncing.
    Syncing,
    /// Node disconnected.
    Disconnected,
    /// App is paused (user-initiated).
    Paused,
}

impl TrayIconState {
    /// Icon name/identifier for each state.
    #[allow(dead_code)]
    pub fn icon_name(&self) -> &'static str {
        match self {
            TrayIconState::Active => "tray-active",
            TrayIconState::Syncing => "tray-syncing",
            TrayIconState::Disconnected => "tray-disconnected",
            TrayIconState::Paused => "tray-paused",
        }
    }

    /// Tooltip text for each state.
    pub fn tooltip(&self) -> &'static str {
        match self {
            TrayIconState::Active => "Commputer - Active",
            TrayIconState::Syncing => "Commputer - Syncing...",
            TrayIconState::Disconnected => "Commputer - Disconnected",
            TrayIconState::Paused => "Commputer - Paused",
        }
    }
}

/// Menu items for the tray context menu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrayMenuItem {
    pub id: String,
    pub label: String,
    pub enabled: bool,
}

/// System tray manager (stub).
pub struct TrayIcon {
    state: TrayIconState,
    menu_items: Vec<TrayMenuItem>,
    visible: bool,
}

impl TrayIcon {
    /// Create a new tray icon (not yet shown).
    pub fn new() -> Self {
        Self {
            state: TrayIconState::Disconnected,
            menu_items: Self::default_menu(),
            visible: false,
        }
    }

    /// Default context menu items.
    fn default_menu() -> Vec<TrayMenuItem> {
        vec![
            TrayMenuItem { id: "show".into(), label: "Show Dashboard".into(), enabled: true },
            TrayMenuItem { id: "status".into(), label: "Status: Disconnected".into(), enabled: false },
            TrayMenuItem { id: "separator".into(), label: "---".into(), enabled: false },
            TrayMenuItem { id: "settings".into(), label: "Settings".into(), enabled: true },
            TrayMenuItem { id: "quit".into(), label: "Quit".into(), enabled: true },
        ]
    }

    /// Show the tray icon.
    pub fn show(&mut self) {
        self.visible = true;
        tracing::info!("System tray icon shown ({})", self.state.tooltip());
    }

    /// Hide the tray icon.
    #[allow(dead_code)]
    pub fn hide(&mut self) {
        self.visible = false;
        tracing::info!("System tray icon hidden");
    }

    /// Whether the tray icon is visible.
    #[allow(dead_code)]
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Update the tray icon state.
    pub fn set_state(&mut self, state: TrayIconState) {
        self.state = state;
        // Update the status menu item.
        if let Some(item) = self.menu_items.iter_mut().find(|i| i.id == "status") {
            item.label = format!("Status: {}", state.tooltip().strip_prefix("Commputer - ").unwrap_or("Unknown"));
        }
        tracing::debug!("Tray icon state changed to {:?}", state);
    }

    /// Get current state.
    #[allow(dead_code)]
    pub fn state(&self) -> TrayIconState {
        self.state
    }

    /// Get menu items (for rendering).
    #[allow(dead_code)]
    pub fn menu_items(&self) -> &[TrayMenuItem] {
        &self.menu_items
    }
}

impl Default for TrayIcon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icon_creation() {
        let tray = TrayIcon::new();
        assert_eq!(tray.state(), TrayIconState::Disconnected);
        assert!(!tray.is_visible());
    }

    #[test]
    fn tray_icon_show_hide() {
        let mut tray = TrayIcon::new();
        tray.show();
        assert!(tray.is_visible());
        tray.hide();
        assert!(!tray.is_visible());
    }

    #[test]
    fn tray_state_update() {
        let mut tray = TrayIcon::new();
        tray.set_state(TrayIconState::Active);
        assert_eq!(tray.state(), TrayIconState::Active);
        // Status menu item should be updated.
        let status = tray.menu_items().iter().find(|i| i.id == "status").unwrap();
        assert!(status.label.contains("Active"));
    }

    #[test]
    fn tray_icon_names() {
        assert_eq!(TrayIconState::Active.icon_name(), "tray-active");
        assert_eq!(TrayIconState::Syncing.icon_name(), "tray-syncing");
        assert_eq!(TrayIconState::Disconnected.icon_name(), "tray-disconnected");
        assert_eq!(TrayIconState::Paused.icon_name(), "tray-paused");
    }

    #[test]
    fn tray_tooltips() {
        assert!(TrayIconState::Active.tooltip().contains("Active"));
        assert!(TrayIconState::Syncing.tooltip().contains("Syncing"));
    }

    #[test]
    fn default_menu_items() {
        let tray = TrayIcon::new();
        let items = tray.menu_items();
        assert!(items.len() >= 4);
        assert!(items.iter().any(|i| i.id == "show"));
        assert!(items.iter().any(|i| i.id == "quit"));
    }
}
