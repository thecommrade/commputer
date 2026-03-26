//! Item 188: Auto-start implementation.
//! Platform-specific stubs for Linux (systemd user), macOS (LaunchAgent), Windows (registry).

use std::path::PathBuf;

/// Auto-start manager for the Commputer desktop app.
pub struct AutoStart {
    app_name: String,
    exe_path: PathBuf,
}

/// Which platform auto-start method to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOS,
    Windows,
}

impl Platform {
    /// Detect the current platform at compile time.
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "macos") {
            Platform::MacOS
        } else {
            Platform::Windows
        }
    }
}

impl AutoStart {
    /// Create a new auto-start manager.
    pub fn new(app_name: &str, exe_path: PathBuf) -> Self {
        Self {
            app_name: app_name.to_string(),
            exe_path,
        }
    }

    /// Enable auto-start for the current platform.
    pub fn enable(&self) -> Result<(), String> {
        match Platform::current() {
            Platform::Linux => self.enable_linux(),
            Platform::MacOS => self.enable_macos(),
            Platform::Windows => self.enable_windows(),
        }
    }

    /// Disable auto-start for the current platform.
    pub fn disable(&self) -> Result<(), String> {
        match Platform::current() {
            Platform::Linux => self.disable_linux(),
            Platform::MacOS => self.disable_macos(),
            Platform::Windows => self.disable_windows(),
        }
    }

    /// Check if auto-start is enabled.
    pub fn is_enabled(&self) -> bool {
        match Platform::current() {
            Platform::Linux => self.linux_service_path().exists(),
            Platform::MacOS => self.macos_plist_path().exists(),
            Platform::Windows => false, // Would check registry
        }
    }

    // --- Linux: systemd user service ---

    fn linux_service_path(&self) -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config/systemd/user")
            .join(format!("{}.service", self.app_name))
    }

    fn enable_linux(&self) -> Result<(), String> {
        let service_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config/systemd/user");
        std::fs::create_dir_all(&service_dir)
            .map_err(|e| format!("failed to create systemd dir: {e}"))?;

        let unit = format!(
            "[Unit]\n\
             Description=Commputer Desktop App\n\
             After=network.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={exe}\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            exe = self.exe_path.display()
        );

        let path = self.linux_service_path();
        std::fs::write(&path, unit)
            .map_err(|e| format!("failed to write service file: {e}"))?;

        // Note: In production, would run `systemctl --user enable commputer-desktop`.
        tracing::info!("Created systemd user service at {}", path.display());
        Ok(())
    }

    fn disable_linux(&self) -> Result<(), String> {
        let path = self.linux_service_path();
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("failed to remove service file: {e}"))?;
        }
        Ok(())
    }

    // --- macOS: LaunchAgent ---

    fn macos_plist_path(&self) -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/LaunchAgents")
            .join(format!("com.commputer.{}.plist", self.app_name))
    }

    fn enable_macos(&self) -> Result<(), String> {
        let agents_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents_dir)
            .map_err(|e| format!("failed to create LaunchAgents dir: {e}"))?;

        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
               <key>Label</key>\n\
               <string>com.commputer.{name}</string>\n\
               <key>ProgramArguments</key>\n\
               <array>\n\
                 <string>{exe}</string>\n\
               </array>\n\
               <key>RunAtLoad</key>\n\
               <true/>\n\
               <key>KeepAlive</key>\n\
               <false/>\n\
             </dict>\n\
             </plist>\n",
            name = self.app_name,
            exe = self.exe_path.display()
        );

        let path = self.macos_plist_path();
        std::fs::write(&path, plist)
            .map_err(|e| format!("failed to write plist: {e}"))?;

        tracing::info!("Created LaunchAgent at {}", path.display());
        Ok(())
    }

    fn disable_macos(&self) -> Result<(), String> {
        let path = self.macos_plist_path();
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("failed to remove plist: {e}"))?;
        }
        Ok(())
    }

    // --- Windows: registry (stub) ---

    fn enable_windows(&self) -> Result<(), String> {
        // In production, would write to:
        // HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run
        tracing::info!(
            "Windows auto-start: would add registry key for {}",
            self.exe_path.display()
        );
        Ok(())
    }

    fn disable_windows(&self) -> Result<(), String> {
        tracing::info!("Windows auto-start: would remove registry key");
        Ok(())
    }

    /// Generate the systemd unit content (for testing without disk writes).
    pub fn systemd_unit_content(&self) -> String {
        format!(
            "[Unit]\n\
             Description=Commputer Desktop App\n\
             After=network.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={exe}\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            exe = self.exe_path.display()
        )
    }

    /// Generate the macOS plist content (for testing without disk writes).
    pub fn macos_plist_content(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
               <key>Label</key>\n\
               <string>com.commputer.{name}</string>\n\
               <key>ProgramArguments</key>\n\
               <array>\n\
                 <string>{exe}</string>\n\
               </array>\n\
               <key>RunAtLoad</key>\n\
               <true/>\n\
               <key>KeepAlive</key>\n\
               <false/>\n\
             </dict>\n\
             </plist>\n",
            name = self.app_name,
            exe = self.exe_path.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_start_creation() {
        let auto = AutoStart::new("commputer-desktop", PathBuf::from("/usr/bin/commputer-desktop"));
        assert_eq!(auto.app_name, "commputer-desktop");
    }

    #[test]
    fn platform_detection() {
        let platform = Platform::current();
        // Just verify it returns one of the variants without panicking.
        assert!(matches!(platform, Platform::Linux | Platform::MacOS | Platform::Windows));
    }

    #[test]
    fn systemd_unit_content() {
        let auto = AutoStart::new("commputer", PathBuf::from("/usr/bin/commputer-desktop"));
        let content = auto.systemd_unit_content();
        assert!(content.contains("[Unit]"));
        assert!(content.contains("ExecStart=/usr/bin/commputer-desktop"));
        assert!(content.contains("[Install]"));
    }

    #[test]
    fn macos_plist_content() {
        let auto = AutoStart::new("desktop", PathBuf::from("/Applications/Commputer.app/Contents/MacOS/commputer"));
        let content = auto.macos_plist_content();
        assert!(content.contains("com.commputer.desktop"));
        assert!(content.contains("RunAtLoad"));
        assert!(content.contains("<true/>"));
    }
}
