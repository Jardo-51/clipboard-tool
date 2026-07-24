//! Start-on-login integration via the `auto-launch` crate.
//!
//! Cross-platform: writes a `~/.config/autostart/*.desktop` entry on Linux, a
//! `Run` registry value on Windows, and a LaunchAgent plist on macOS.

use auto_launch::{AutoLaunch, AutoLaunchBuilder, MacOSLaunchMode};

const APP_NAME: &str = "clipboard-tool";

/// Build an [`AutoLaunch`] targeting the currently running executable.
fn handle() -> Result<AutoLaunch, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own path: {e}"))?;
    AutoLaunchBuilder::new()
        .set_app_name(APP_NAME)
        .set_app_path(&exe.to_string_lossy())
        // On macOS, register as a LaunchAgent (plist in ~/Library/LaunchAgents).
        .set_macos_launch_mode(MacOSLaunchMode::LaunchAgent)
        .build()
        .map_err(|e| format!("failed to configure autostart: {e}"))
}

pub fn is_enabled() -> bool {
    handle()
        .and_then(|a| a.is_enabled().map_err(|e| e.to_string()))
        .unwrap_or(false)
}

pub fn enable() -> Result<(), String> {
    handle()?.enable().map_err(|e| e.to_string())
}

pub fn disable() -> Result<(), String> {
    handle()?.disable().map_err(|e| e.to_string())
}

/// Flip autostart and return the new state (`true` = now enabled).
pub fn toggle() -> Result<bool, String> {
    if is_enabled() {
        disable()?;
        Ok(false)
    } else {
        enable()?;
        Ok(true)
    }
}
