//! System-tray icon with a context menu (Show history / Start on login /
//! Clear history / Quit).
//!
//! Linux is the primary target and is implemented here. `tray-icon` requires a
//! running GTK main loop, and eframe already owns the winit event loop, so the
//! tray runs on its own dedicated GTK thread and communicates with the UI only
//! through the shared [`crate::Shared`] state and the egui [`egui::Context`].
//!
//! Windows/macOS require the tray to share the winit event loop (they can't run
//! a second one on a background thread); that integration is deferred, so on
//! those platforms `spawn` is a no-op for now.

use std::sync::Arc;

use crate::Shared;

#[cfg(target_os = "linux")]
pub fn spawn(shared: Arc<Shared>, ctx: egui::Context) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use gtk::glib;
    use tray_icon::{
        menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        TrayIconBuilder,
    };

    std::thread::spawn(move || {
        if gtk::init().is_err() {
            eprintln!("tray: failed to initialize GTK; running without a tray icon");
            return;
        }

        let menu = Menu::new();
        let show_item = MenuItem::new("Show history\t(Ctrl+Shift+V)", true, None);
        let autostart_item =
            CheckMenuItem::new("Start on login", true, crate::autostart::is_enabled(), None);
        let clear_item = MenuItem::new("Clear history", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        if let Err(e) = menu.append_items(&[
            &show_item,
            &autostart_item,
            &clear_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ]) {
            eprintln!("tray: failed to build menu: {e}");
            return;
        }

        // Keep the returned TrayIcon alive for the lifetime of the GTK loop;
        // dropping it removes the icon.
        let _tray = match TrayIconBuilder::new()
            .with_tooltip("clipboard-tool")
            .with_icon(make_icon())
            .with_menu(Box::new(menu))
            .build()
        {
            Ok(t) => t,
            Err(e) => {
                eprintln!("tray: failed to create tray icon: {e}");
                return;
            }
        };

        let show_id = show_item.id().clone();
        let clear_id = clear_item.id().clone();
        let quit_id = quit_item.id().clone();
        let autostart_id = autostart_item.id().clone();

        let menu_rx = MenuEvent::receiver();
        // Poll menu events from within the GTK main loop (same thread, so the
        // non-Send muda item handles are safe to touch here).
        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = menu_rx.try_recv() {
                let id = event.id();
                if id == &show_id {
                    shared.show_requested.store(true, Ordering::SeqCst);
                    ctx.request_repaint();
                } else if id == &clear_id {
                    if let Ok(mut h) = shared.history.lock() {
                        h.clear();
                    }
                    shared.dirty.store(true, Ordering::SeqCst);
                } else if id == &autostart_id {
                    match crate::autostart::toggle() {
                        Ok(now) => autostart_item.set_checked(now),
                        Err(e) => {
                            eprintln!("tray: autostart toggle failed: {e}");
                            autostart_item.set_checked(crate::autostart::is_enabled());
                        }
                    }
                } else if id == &quit_id {
                    crate::save_history(&shared);
                    std::process::exit(0);
                }
            }
            glib::ControlFlow::Continue
        });

        gtk::main();
    });
}

#[cfg(not(target_os = "linux"))]
pub fn spawn(_shared: Arc<Shared>, _ctx: egui::Context) {
    // Tray support on Windows/macOS needs winit-event-loop integration; deferred.
    eprintln!("tray: not yet implemented on this platform; running without a tray icon");
}

/// A simple 32×32 clipboard glyph so we don't need to bundle an image asset.
#[cfg(target_os = "linux")]
fn make_icon() -> tray_icon::Icon {
    const S: u32 = 32;
    let mut rgba = vec![0u8; (S * S * 4) as usize];
    let put = |buf: &mut [u8], x: u32, y: u32, c: [u8; 4]| {
        let i = ((y * S + x) * 4) as usize;
        buf[i..i + 4].copy_from_slice(&c);
    };
    let board = [0x3bu8, 0x82, 0xf6, 0xff]; // blue body
    let paper = [0xf8u8, 0xfa, 0xfc, 0xff]; // light "page"
    let clip = [0x1eu8, 0x40, 0xafu8, 0xff]; // dark clip
    for y in 0..S {
        for x in 0..S {
            if (5..27).contains(&x) && (4..29).contains(&y) {
                put(&mut rgba, x, y, board);
            }
            if (8..24).contains(&x) && (9..26).contains(&y) {
                put(&mut rgba, x, y, paper);
            }
            if (12..20).contains(&x) && (3..7).contains(&y) {
                put(&mut rgba, x, y, clip);
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, S, S).expect("valid tray icon")
}
