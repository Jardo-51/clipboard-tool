//! Wayland global hotkey via the XDG **GlobalShortcuts** portal.
//!
//! On a Wayland session the X11 key-grab used by `global-hotkey` doesn't work,
//! so we register our shortcut through the desktop portal instead (`ashpd`).
//! This is implemented and supported on KDE Plasma and GNOME 48+, but **not**
//! by wlroots-based compositors (Sway/Hyprland/Niri), which ship no
//! GlobalShortcuts backend. When the portal is unavailable we log a hint and
//! the user can still open the popup from the tray icon's "Show history".
//!
//! NOTE: this path requires a live Wayland session with a portal backend and
//! cannot be exercised in a headless CI environment; it is verified by
//! compilation here and needs on-device testing.

/// Identifier we register the shortcut under, matched on activation.
#[cfg(target_os = "linux")]
const SHORTCUT_ID: &str = "show-history";

/// Spawn a background listener that calls `on_activate` each time the user
/// triggers the app's global shortcut through the portal. `trigger` is a
/// best-effort preferred accelerator (e.g. "CTRL+SHIFT+v"); the compositor may
/// ignore it and let the user bind their own.
#[cfg(target_os = "linux")]
pub fn spawn_portal_hotkey<F>(trigger: Option<String>, on_activate: F)
where
    F: Fn() + Send + 'static,
{
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("wayland hotkey: could not start async runtime: {e}");
                return;
            }
        };

        if let Err(e) = runtime.block_on(run_portal(trigger, on_activate)) {
            eprintln!(
                "wayland hotkey: GlobalShortcuts portal unavailable ({e}). \
                 Open the popup from the tray icon's \"Show history\" instead. \
                 (wlroots compositors such as Sway/Hyprland don't implement this \
                 portal; KDE and GNOME 48+ do.)"
            );
        }
    });
}

#[cfg(target_os = "linux")]
async fn run_portal<F>(trigger: Option<String>, on_activate: F) -> Result<(), ashpd::Error>
where
    F: Fn(),
{
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::StreamExt;

    let shortcuts = GlobalShortcuts::new().await?;
    let session = shortcuts.create_session(Default::default()).await?;

    let mut shortcut = NewShortcut::new(SHORTCUT_ID, "Open clipboard history");
    if let Some(t) = trigger.as_deref() {
        shortcut = shortcut.preferred_trigger(t);
    }

    // On first run this may show a portal dialog to confirm the binding.
    shortcuts
        .bind_shortcuts(&session, &[shortcut], None, Default::default())
        .await?;

    let mut activations = shortcuts.receive_activated().await?;
    while let Some(event) = activations.next().await {
        if event.shortcut_id() == SHORTCUT_ID {
            on_activate();
        }
    }
    Ok(())
}

/// Convert a `global-hotkey`-style string ("ctrl+shift+KeyV") into the portal's
/// preferred-trigger syntax ("CTRL+SHIFT+v"). Best-effort: returns `None` if it
/// can't produce something sensible, in which case the portal assigns/asks.
/// (Available on all platforms so the call site compiles; only used on Wayland.)
pub fn to_portal_trigger(hotkey: &str) -> Option<String> {
    let mut parts = Vec::new();
    for token in hotkey.split('+') {
        let token = token.trim();
        let mapped = match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "CTRL".to_string(),
            "shift" => "SHIFT".to_string(),
            "alt" | "option" => "ALT".to_string(),
            "super" | "meta" | "cmd" | "command" | "win" => "SUPER".to_string(),
            _ => {
                // A key code such as "KeyV" or "Digit1" → the bare key ("v", "1").
                let key = token
                    .strip_prefix("Key")
                    .or_else(|| token.strip_prefix("Digit"))
                    .unwrap_or(token);
                if key.is_empty() {
                    return None;
                }
                key.to_ascii_lowercase()
            }
        };
        parts.push(mapped);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("+"))
    }
}

/// No-op on non-Linux platforms (Wayland is Linux-only).
#[cfg(not(target_os = "linux"))]
pub fn spawn_portal_hotkey<F>(_trigger: Option<String>, _on_activate: F)
where
    F: Fn() + Send + 'static,
{
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_standard_hotkey() {
        assert_eq!(
            to_portal_trigger("ctrl+shift+KeyV").as_deref(),
            Some("CTRL+SHIFT+v")
        );
    }

    #[test]
    fn maps_super_and_digits() {
        assert_eq!(
            to_portal_trigger("super+Digit1").as_deref(),
            Some("SUPER+1")
        );
    }

    #[test]
    fn passes_through_bare_key() {
        assert_eq!(to_portal_trigger("F5").as_deref(), Some("f5"));
    }
}
