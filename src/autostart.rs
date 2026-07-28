//! Start-on-login integration via the `auto-launch` crate.
//!
//! Cross-platform: writes a `~/.config/autostart/*.desktop` entry on Linux, a
//! `Run` registry value on Windows, and a LaunchAgent plist on macOS.

use std::path::PathBuf;

use auto_launch::{AutoLaunch, AutoLaunchBuilder, MacOSLaunchMode};

const APP_NAME: &str = "clipboard-tool";

/// Environment variable a packaging layer can set to override the path written
/// into the autostart entry. See [`exe_path`].
const EXE_OVERRIDE_VAR: &str = "CLIPBOARD_TOOL_EXE";

/// The path autostart should launch.
///
/// Normally that is `current_exe()`, but it can't be used unconditionally:
/// wrapper-based packaging puts a launcher script at the user-facing path and
/// moves the real ELF aside, and `current_exe()` resolves through
/// `/proc/self/exe` to the moved ELF. This project's own Nix package does
/// exactly that (`wrapProgram` → `.clipboard-tool-wrapped`), and the wrapper is
/// what sets `LD_LIBRARY_PATH` and the GL/Vulkan driver variables the tray,
/// input injection and wgpu popup all need. Writing the inner path into the
/// `.desktop` entry would produce an autostarted instance that silently fails to
/// dlopen those libraries.
///
/// So prefer `CLIPBOARD_TOOL_EXE` when the packaging sets it and it still
/// points at something that exists; otherwise fall back to `current_exe()`.
fn exe_path() -> Result<PathBuf, String> {
    if let Some(override_path) = std::env::var_os(EXE_OVERRIDE_VAR) {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Ok(path);
        }
        eprintln!(
            "autostart: {EXE_OVERRIDE_VAR} is set to {} which does not exist; \
             falling back to the running executable",
            path.display()
        );
    }
    std::env::current_exe().map_err(|e| format!("cannot find own path: {e}"))
}

/// Build an [`AutoLaunch`] targeting the executable users are meant to invoke.
fn handle() -> Result<AutoLaunch, String> {
    let exe = exe_path()?;
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
