//! Input-injection backend: place text on the clipboard and synthesize a
//! paste (Ctrl+V, or Cmd+V on macOS) into whatever window currently has focus.

use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings};

/// Paste modifier: Command on macOS, Control everywhere else.
#[cfg(target_os = "macos")]
const PASTE_MODIFIER: Key = Key::Meta;
#[cfg(not(target_os = "macos"))]
const PASTE_MODIFIER: Key = Key::Control;

/// How long to keep owning the X11 selection after issuing the keystroke, so
/// the target window can read the pasted data before we relinquish ownership.
const SELECTION_GRACE: Duration = Duration::from_millis(150);

pub trait InputInjector: Send + Sync {
    /// Put `text` on the clipboard and paste it into the focused window.
    ///
    /// The clipboard is always set first, so even if key synthesis fails
    /// (e.g. a restricted Wayland compositor) the value is available for the
    /// user to paste manually — the caller surfaces that as a hint.
    fn paste(&self, text: &str) -> Result<(), String>;
}

/// Cross-platform backend using `arboard` (clipboard) + `enigo` (keystrokes).
/// Works on X11, Windows, macOS, and — experimentally — Wayland via libei.
pub struct EnigoInjector;

impl EnigoInjector {
    pub fn new() -> Self {
        Self
    }
}

impl InputInjector for EnigoInjector {
    fn paste(&self, text: &str) -> Result<(), String> {
        // 1. Set the clipboard. Keep the handle alive until after the keystroke
        //    so X11 selection ownership is served during the paste.
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
        clipboard
            .set_text(text.to_owned())
            .map_err(|e| format!("failed to set clipboard: {e}"))?;

        // 2. Synthesize the paste shortcut into the focused window.
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| format!("input backend unavailable: {e}"))?;
        enigo
            .key(PASTE_MODIFIER, Direction::Press)
            .map_err(|e| e.to_string())?;
        let v = enigo.key(Key::Unicode('v'), Direction::Click);
        // Always release the modifier, even if the 'v' click failed, so we
        // never leave Ctrl/Cmd stuck down.
        let _ = enigo.key(PASTE_MODIFIER, Direction::Release);
        v.map_err(|e| e.to_string())?;

        // 3. Hold clipboard ownership briefly so the target can read it (X11).
        std::thread::sleep(SELECTION_GRACE);
        drop(clipboard);
        Ok(())
    }
}
