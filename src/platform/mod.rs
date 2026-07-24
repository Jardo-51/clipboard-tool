//! Platform abstraction for OS integration that differs across Linux
//! (X11/Wayland), Windows, and macOS.
//!
//! Phase 4 introduces the input-injection backend. The trait is intentionally
//! object-safe and `Send + Sync` so a single boxed instance can live in the
//! shared app state and be called from a background thread (injection must
//! happen off the UI thread, after the popup has yielded focus).

mod inject;

pub use inject::InputInjector;

/// Build the input injector appropriate for the current OS/session.
///
/// Phase 5 will branch here on the Linux display server (X11 vs Wayland
/// portal). For now every platform uses the `enigo`-based backend.
pub fn default_injector() -> Box<dyn InputInjector> {
    Box::new(inject::EnigoInjector::new())
}

/// Detected Linux session type, used to pick backends. Harmless on non-Linux
/// (always returns [`SessionType::Other`]).
#[allow(dead_code)] // consumed by the Wayland backend selection in Phase 5
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    X11,
    Wayland,
    Other,
}

#[allow(dead_code)] // consumed by the Wayland backend selection in Phase 5
pub fn detect_session() -> SessionType {
    // `XDG_SESSION_TYPE` is the primary signal; `WAYLAND_DISPLAY` is a fallback
    // for environments that don't set it.
    match std::env::var("XDG_SESSION_TYPE").ok().as_deref() {
        Some("wayland") => SessionType::Wayland,
        Some("x11") => SessionType::X11,
        _ => {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                SessionType::Wayland
            } else if std::env::var_os("DISPLAY").is_some() {
                SessionType::X11
            } else {
                SessionType::Other
            }
        }
    }
}
