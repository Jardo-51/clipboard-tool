//! clipboard-tool — lightweight cross-platform clipboard history manager.
//!
//! Phase 1–2 milestone: a background clipboard watcher records copied text into
//! a capped history, and a global Ctrl+Shift+V shows a centered egui popup
//! listing recent items (arrow keys navigate, Enter selects, Esc dismisses).
//! Selection currently copies back to the clipboard; synthetic paste injection
//! (enigo) lands in Phase 4.

// Hide the console window on Windows in release builds.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod history;
mod platform;
mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};

use history::HistoryStore;

const HISTORY_CAPACITY: usize = 100;
const POPUP_WIDTH: f32 = 460.0;
const POPUP_HEIGHT: f32 = 340.0;
/// Grace period after hiding the popup before synthesizing the paste, so the
/// OS can return focus to the previously active window.
const FOCUS_RETURN_DELAY: Duration = Duration::from_millis(120);

/// Shared application state between the clipboard-watcher thread, the
/// hotkey-listener thread, and the egui UI thread.
struct Shared {
    history: Mutex<HistoryStore>,
    /// Set by the hotkey thread; consumed by the UI to show the popup.
    show_requested: AtomicBool,
    /// Backend that pastes a chosen item into the focused window.
    injector: Box<dyn platform::InputInjector>,
}

fn main() -> eframe::Result<()> {
    // Handle one-shot CLI commands (autostart management) before launching the UI.
    if let Some(code) = run_cli() {
        std::process::exit(code);
    }

    let shared = Arc::new(Shared {
        history: Mutex::new(HistoryStore::new(HISTORY_CAPACITY)),
        show_requested: AtomicBool::new(false),
        injector: platform::default_injector(),
    });

    // --- Clipboard watcher (Phase 2) -------------------------------------
    spawn_clipboard_watcher(shared.clone());

    // --- Global hotkey: Ctrl+Shift+V (Phase 1) ---------------------------
    // The manager must outlive the program, so keep it alive on the stack.
    let manager = GlobalHotKeyManager::new().expect("failed to init global hotkey manager");
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyV);
    manager
        .register(hotkey)
        .expect("failed to register Ctrl+Shift+V");
    let hotkey_id = hotkey.id();

    // --- egui popup ------------------------------------------------------
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([POPUP_WIDTH, POPUP_HEIGHT])
            .with_min_inner_size([POPUP_WIDTH, POPUP_HEIGHT])
            .with_decorations(false)
            .with_always_on_top()
            .with_resizable(false)
            .with_visible(false),
        ..Default::default()
    };

    eframe::run_native(
        "clipboard-tool",
        native_options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();

            // Wake the event loop whenever a hotkey fires, even while hidden.
            let shared_for_hotkeys = shared.clone();
            let ctx_for_hotkeys = ctx.clone();
            std::thread::spawn(move || {
                let receiver = GlobalHotKeyEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if event.id == hotkey_id
                        && event.state == global_hotkey::HotKeyState::Pressed
                    {
                        shared_for_hotkeys.show_requested.store(true, Ordering::SeqCst);
                        ctx_for_hotkeys.request_repaint();
                    }
                }
            });

            // Tray icon (its own GTK loop on Linux; no-op elsewhere for now).
            tray::spawn(shared.clone(), ctx.clone());

            Ok(Box::new(PopupApp::new(shared.clone())))
        }),
    )?;

    drop(manager);
    Ok(())
}

/// Handle one-shot command-line subcommands. Returns `Some(exit_code)` if a
/// command was handled and the process should exit, or `None` to launch the UI.
fn run_cli() -> Option<i32> {
    let arg = std::env::args().nth(1)?;
    match arg.as_str() {
        "--enable-autostart" => Some(match autostart::enable() {
            Ok(()) => {
                println!("Autostart enabled.");
                0
            }
            Err(e) => {
                eprintln!("Failed to enable autostart: {e}");
                1
            }
        }),
        "--disable-autostart" => Some(match autostart::disable() {
            Ok(()) => {
                println!("Autostart disabled.");
                0
            }
            Err(e) => {
                eprintln!("Failed to disable autostart: {e}");
                1
            }
        }),
        "--autostart-status" => {
            println!(
                "Autostart is {}.",
                if autostart::is_enabled() {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            Some(0)
        }
        "--help" | "-h" => {
            println!(
                "clipboard-tool — clipboard history manager\n\n\
                 Run with no arguments to start the background daemon and tray.\n\
                 Press Ctrl+Shift+V to open the history popup.\n\n\
                 Commands:\n  \
                 --enable-autostart    Start automatically on login\n  \
                 --disable-autostart   Stop starting on login\n  \
                 --autostart-status    Show whether autostart is enabled\n  \
                 --help, -h            Show this help"
            );
            Some(0)
        }
        other => {
            eprintln!("Unknown argument: {other}\nRun with --help for usage.");
            Some(2)
        }
    }
}

/// Spawns a thread that watches the OS clipboard and appends text changes to
/// the shared history.
fn spawn_clipboard_watcher(shared: Arc<Shared>) {
    use clipboard_master::{CallbackResult, ClipboardHandler, Master};

    struct Handler {
        shared: Arc<Shared>,
        clipboard: arboard::Clipboard,
    }

    impl ClipboardHandler for Handler {
        fn on_clipboard_change(&mut self) -> CallbackResult {
            if let Ok(text) = self.clipboard.get_text() {
                if let Ok(mut hist) = self.shared.history.lock() {
                    hist.push(text);
                }
            }
            CallbackResult::Next
        }

        fn on_clipboard_error(&mut self, error: std::io::Error) -> CallbackResult {
            eprintln!("clipboard watch error: {error}");
            CallbackResult::Next
        }
    }

    std::thread::spawn(move || {
        let clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to open clipboard: {e}");
                return;
            }
        };
        let handler = Handler { shared, clipboard };
        match Master::new(handler) {
            Ok(mut master) => {
                if let Err(e) = master.run() {
                    eprintln!("clipboard master stopped: {e}");
                }
            }
            Err(e) => eprintln!("failed to start clipboard master: {e}"),
        }
    });
}

struct PopupApp {
    shared: Arc<Shared>,
    selected: usize,
    visible: bool,
}

impl PopupApp {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            selected: 0,
            visible: false,
        }
    }

    fn show_popup(&mut self, ctx: &egui::Context) {
        self.visible = true;
        self.selected = 0;
        center_on_screen(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide_popup(&mut self, ctx: &egui::Context) {
        self.visible = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// Commit the currently selected item: hide the popup so focus returns to
    /// the previously active window, then (off the UI thread) place the item on
    /// the clipboard and synthesize a paste into that window.
    fn commit_selection(&mut self, ctx: &egui::Context) {
        let chosen = self
            .shared
            .history
            .lock()
            .ok()
            .and_then(|h| h.get(self.selected).cloned());

        // Hide first — the target app must regain focus before we paste.
        self.hide_popup(ctx);

        if let Some(text) = chosen {
            let shared = self.shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(FOCUS_RETURN_DELAY);
                if let Err(e) = shared.injector.paste(&text) {
                    eprintln!(
                        "auto-paste failed ({e}); the item is on the clipboard — press \
                         Ctrl+V to paste it manually."
                    );
                }
            });
        }
    }
}

impl eframe::App for PopupApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if self.shared.show_requested.swap(false, Ordering::SeqCst) {
            self.show_popup(&ctx);
        }

        if !self.visible {
            return;
        }

        // Snapshot the history once per frame so the lock isn't held while we
        // borrow `self` mutably during rendering.
        let items: Vec<String> = self
            .shared
            .history
            .lock()
            .map(|h| h.iter().cloned().collect())
            .unwrap_or_default();
        let len = items.len();

        // --- Keyboard handling ---
        let mut commit = false;
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                self.visible = false; // deferred hide below
            }
            if i.key_pressed(egui::Key::Enter) {
                commit = true;
            }
            if len > 0 {
                if i.key_pressed(egui::Key::ArrowDown) {
                    self.selected = (self.selected + 1) % len;
                }
                if i.key_pressed(egui::Key::ArrowUp) {
                    self.selected = (self.selected + len - 1) % len;
                }
            }
        });

        // Esc requested a hide.
        if !self.visible {
            self.hide_popup(&ctx);
            return;
        }
        if commit {
            self.commit_selection(&ctx);
            return;
        }

        // Dismiss when the popup loses focus (click-away / alt-tab).
        if ctx.input(|i| i.viewport().focused == Some(false)) {
            self.hide_popup(&ctx);
            return;
        }

        // --- Render (into the central Ui eframe provides) ---
        ui.heading("Clipboard history");
        ui.separator();
        if len == 0 {
            ui.label("No items yet — copy something.");
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (idx, item) in items.iter().enumerate() {
                let selected = idx == self.selected;
                let resp = ui.selectable_label(selected, one_line_preview(item));
                if resp.clicked() {
                    self.selected = idx;
                    commit = true;
                }
                if selected {
                    resp.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });

        if commit {
            self.commit_selection(&ctx);
        }
    }
}

/// Collapse a clipboard entry to a single trimmed preview line for the menu.
fn one_line_preview(s: &str) -> String {
    const MAX: usize = 80;
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > MAX {
        let truncated: String = flat.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        flat
    }
}

/// Position the popup in the middle of the primary monitor.
fn center_on_screen(ctx: &egui::Context) {
    let monitor = ctx.input(|i| i.viewport().monitor_size);
    if let Some(size) = monitor {
        let pos = egui::pos2(
            ((size.x - POPUP_WIDTH) / 2.0).max(0.0),
            ((size.y - POPUP_HEIGHT) / 2.0).max(0.0),
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
    }
}
