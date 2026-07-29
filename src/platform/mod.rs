//! Platform abstraction for OS integration that differs across Linux
//! (X11/Wayland), Windows, and macOS.
//!
//! The input-injection trait is intentionally object-safe and `Send + Sync` so
//! a single boxed instance can live in the shared app state and be called from
//! a background thread (injection must happen off the UI thread, after the
//! popup has yielded focus).

mod inject;

pub use inject::InputInjector;

/// Build the input injector appropriate for the current OS/session.
///
/// Every platform uses the `enigo`-based backend, and deliberately so: `enigo`
/// already selects between the Wayland virtual-keyboard protocol and X11 at
/// runtime, so branching on [`detect_session`] here would only second-guess it.
/// A libei backend for GNOME/KDE is the one case that would need its own arm —
/// see [`inject::EnigoInjector`] for why it isn't wired up.
pub fn default_injector() -> Box<dyn InputInjector> {
    Box::new(inject::EnigoInjector)
}

/// Detected Linux session type, used to pick backends. Harmless on non-Linux
/// (always returns [`SessionType::Other`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    X11,
    Wayland,
    Other,
}

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
