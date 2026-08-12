//! clipboard-tool — lightweight cross-platform clipboard history manager.
//!
//! A background thread watches the OS clipboard and records copied text into a
//! capped, de-duplicating history. A global hotkey (`Ctrl+Shift+V` by default)
//! shows a centered egui popup of the recent items: arrow keys navigate, Enter
//! puts the chosen entry back on the clipboard and synthesizes a paste into the
//! window that had focus, the trash icon on a row drops that entry, the star
//! icon pins it above the rest, and Esc or clicking away dismisses it. A tray
//! icon offers the same actions, and the history is restored across restarts
//! unless `persist` is turned off in `config.toml`.
//!
//! This module owns the shared state and the wiring between those threads; the
//! pieces live in [`history`], [`persist`], [`config`], [`tray`], [`autostart`],
//! [`wayland`] and [`platform`].

// Hide the console window on Windows in release builds.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod autostart;
mod config;
mod history;
mod persist;
mod platform;
mod tray;
mod wayland;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::Config;

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};

use history::{EntryKind, HistoryStore};

/// Wide enough that the preview keeps roughly the room it had before the star
/// button joined the trash icon at the end of each row.
const POPUP_WIDTH: f32 = 520.0;
const POPUP_HEIGHT: f32 = 340.0;
/// Padding between the popup's contents and the window edge, in points. The
/// window is undecorated, so nothing else keeps the rows off the border.
const POPUP_MARGIN: i8 = 8;
/// Grace period after hiding the popup before synthesizing the paste, so the
/// OS can return focus to the previously active window.
const FOCUS_RETURN_DELAY: Duration = Duration::from_millis(120);
/// How often the background thread flushes a dirty history to disk.
const PERSIST_INTERVAL: Duration = Duration::from_secs(5);
/// Longest preview line shown per history row, in characters.
const PREVIEW_MAX_CHARS: usize = 80;
/// Glyph for the per-row delete button. U+1F5D1 is carried by the bundled
/// `emoji-icon-font`, which egui's default fonts fall back to.
const DELETE_ICON: &str = "🗑";
/// Glyphs for the per-row favorite toggle: filled when the item is starred,
/// outlined when it isn't. U+2605/U+2606 come from the same `emoji-icon-font` as
/// [`DELETE_ICON`] — the emoji star (U+2B50) is in `NotoEmoji` but has no
/// outlined counterpart there, so the pair would not match.
const FAVORITE_ICON_ON: &str = "★";
const FAVORITE_ICON_OFF: &str = "☆";
/// Glyph marking a row whose text is a path the user copied in their file
/// manager, rather than text they copied as text. Without it `/home/me/notes`
/// reads exactly like the same string copied out of a terminal, and the two
/// come from different places. U+1F4C1 is carried by `NotoEmoji`, the other font
/// egui's defaults fall back to besides the `emoji-icon-font` that supplies
/// [`DELETE_ICON`].
const PATH_ICON: &str = "📁";
/// Breathing room between the delete icons and whatever is to their right — the
/// scroll bar when the list overflows, the window margin when it doesn't.
const ROW_TRAILING_GAP: f32 = 6.0;

/// Shared application state between the clipboard-watcher thread, the
/// hotkey-listener thread, the tray thread, and the egui UI thread.
struct Shared {
    history: Mutex<HistoryStore>,
    /// Set by the hotkey/tray threads; consumed by the UI to show the popup.
    show_requested: AtomicBool,
    /// Backend that pastes a chosen item into the focused window.
    injector: Box<dyn platform::InputInjector>,
    /// Set whenever the history changes; drives throttled persistence.
    dirty: AtomicBool,
    /// Whether history should be saved to disk at all.
    persist: bool,
    /// Where the history JSON lives (None if no data dir is available).
    history_path: Option<PathBuf>,
    /// How to spell the active hotkey in the UI, or `None` when there isn't one
    /// (so the tray doesn't advertise a shortcut that does nothing).
    hotkey_label: Option<String>,
}

fn main() -> eframe::Result<()> {
    // Handle one-shot CLI commands (autostart management) before launching the UI.
    if let Some(code) = run_cli() {
        std::process::exit(code);
    }

    let config = Config::load();
    let history_path = if config.persist {
        persist::history_path()
    } else {
        None
    };

    // Seed the history from disk when persistence is enabled.
    let mut store = HistoryStore::new(config.history_size);
    if let Some(path) = &history_path {
        store.restore(persist::load(path));
    }

    // --- Global hotkey -----------------------------------------------------
    // On X11/Windows/macOS use the `global-hotkey` key grab. On Wayland that
    // doesn't work, so we use the GlobalShortcuts portal instead.
    let use_portal_hotkey = platform::detect_session() == platform::SessionType::Wayland;
    let portal_trigger = wayland::to_portal_trigger(&config.hotkey);

    // The manager must outlive the program, so keep it alive on the stack.
    let hotkey_manager = if use_portal_hotkey {
        None
    } else {
        register_global_hotkey(&config)
    };
    let hotkey_id = hotkey_manager.as_ref().map(|(_, id)| *id);

    // Only name a shortcut in the UI if one is actually live. On the portal path
    // the compositor may hand the user a different binding, so the configured
    // one is a best-effort answer there.
    let hotkey_label =
        (use_portal_hotkey || hotkey_manager.is_some()).then(|| config.hotkey_label());

    let shared = Arc::new(Shared {
        history: Mutex::new(store),
        show_requested: AtomicBool::new(false),
        injector: platform::default_injector(),
        dirty: AtomicBool::new(false),
        persist: config.persist,
        history_path,
        hotkey_label,
    });

    // --- Clipboard watcher ------------------------------------------------
    spawn_clipboard_watcher(shared.clone(), config.record_file_paths);

    // --- Throttled history persistence -------------------------------------
    spawn_persistence(shared.clone());

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

    // Kept for a best-effort flush if the event loop ever returns; the closure
    // takes ownership of `shared`.
    let shared_main = shared.clone();

    eframe::run_native(
        "clipboard-tool",
        native_options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();

            // Wake the event loop whenever the hotkey fires, even while hidden.
            let shared_for_hotkeys = shared.clone();
            let ctx_for_hotkeys = ctx.clone();
            let on_hotkey = move || {
                shared_for_hotkeys
                    .show_requested
                    .store(true, Ordering::SeqCst);
                ctx_for_hotkeys.request_repaint();
            };

            if use_portal_hotkey {
                wayland::spawn_portal_hotkey(portal_trigger, on_hotkey);
            } else if let Some(id) = hotkey_id {
                std::thread::spawn(move || {
                    let receiver = GlobalHotKeyEvent::receiver();
                    while let Ok(event) = receiver.recv() {
                        if event.id == id && event.state == global_hotkey::HotKeyState::Pressed {
                            on_hotkey();
                        }
                    }
                });
            }

            // Tray icon (its own GTK loop on Linux; no-op elsewhere for now).
            tray::spawn(shared.clone(), ctx.clone());

            Ok(Box::new(PopupApp::new(shared.clone())))
        }),
    )?;

    drop(hotkey_manager);
    save_history(&shared_main); // best-effort flush if the loop ever returns
    Ok(())
}

/// Register the configured hotkey with the OS, returning the manager (which
/// must be kept alive for the grab to hold) and the hotkey's event id.
///
/// A failed grab is an expected outcome, not a bug: `Ctrl+Shift+V` is claimed by
/// several clipboard managers and desktop-level shortcuts, and X11's
/// `XGrabKey` refuses a second client with `BadAccess`. Rather than abort the
/// process — which under `panic = "abort"` means a silent `SIGABRT` and no tray
/// icon — degrade the same way the Wayland portal path does: warn, return
/// `None`, and let the user open the popup from the tray.
fn register_global_hotkey(config: &Config) -> Option<(GlobalHotKeyManager, u32)> {
    let where_to_configure = Config::config_path()
        .map(|p| format!(" or pick a different `hotkey` in {}", p.display()))
        .unwrap_or_default();

    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "hotkey: could not initialize the global hotkey manager ({e}). \
                 Open the popup from the tray icon's \"Show history\" instead."
            );
            return None;
        }
    };

    let hotkey = config.parse_hotkey();
    if let Err(e) = manager.register(hotkey) {
        eprintln!(
            "hotkey: could not register '{}' ({e}) — another application most \
             likely already holds that combination. Open the popup from the tray \
             icon's \"Show history\" instead{where_to_configure}.",
            config.hotkey
        );
        return None;
    }

    Some((manager, hotkey.id()))
}

/// Persist the current history to disk if persistence is enabled. Cheap and
/// safe to call from any thread (used by the timer, the tray "Clear", and quit).
fn save_history(shared: &Shared) {
    if !shared.persist {
        return;
    }
    let Some(path) = &shared.history_path else {
        return;
    };
    let items = shared
        .history
        .lock()
        .map(|h| h.snapshot())
        .unwrap_or_default();
    if let Err(e) = persist::save(path, &items) {
        eprintln!("failed to save history to {}: {e}", path.display());
    }
}

/// Write the history out now instead of leaving it to the persistence timer.
///
/// Deletions use this; `push` doesn't. A copy that doesn't survive a crash is a
/// non-event, so recording one can wait for the next [`PERSIST_INTERVAL`] tick.
/// A delete is the opposite: it's the operation a user reaches for specifically
/// to get something — a password, a token — *out* of a file on disk, and
/// leaving it in `history.json` for five seconds, or permanently if the process
/// is killed in that window, fails the thing the button is for.
///
/// The write is a blocking one of up to `history_size` × `MAX_ITEM_BYTES`, and
/// every caller is on a UI thread (egui's or GTK's), so it goes to a thread of
/// its own.
fn flush_history(shared: &Arc<Shared>) {
    // This save supersedes the pending one the caller's `dirty` would have
    // triggered. A change racing the write just re-sets the flag, so the worst
    // case is one redundant save on the next tick, never a lost one.
    shared.dirty.store(false, Ordering::SeqCst);
    let shared = Arc::clone(shared);
    std::thread::spawn(move || save_history(&shared));
}

/// Flush a dirty history to disk at a low frequency, so frequent copies don't
/// each trigger a write.
fn spawn_persistence(shared: Arc<Shared>) {
    if !shared.persist {
        return;
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(PERSIST_INTERVAL);
        if shared.dirty.swap(false, Ordering::SeqCst) {
            save_history(&shared);
        }
    });
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
            // Read-only load: printing help shouldn't create a config file.
            let hotkey = Config::load_without_creating().hotkey_label();
            println!(
                "clipboard-tool — clipboard history manager\n\n\
                 Run with no arguments to start the background daemon and tray.\n\
                 Press {hotkey} to open the history popup.\n\n\
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
///
/// `record_file_paths` is passed in rather than kept on [`Shared`] the way
/// `persist` is: this is the only thread that reads it, so there is nothing to
/// share.
fn spawn_clipboard_watcher(shared: Arc<Shared>, record_file_paths: bool) {
    use clipboard_master::{CallbackResult, ClipboardHandler, Master};

    struct Handler {
        shared: Arc<Shared>,
        clipboard: arboard::Clipboard,
        record_file_paths: bool,
    }

    impl ClipboardHandler for Handler {
        fn on_clipboard_change(&mut self) -> CallbackResult {
            if let Some((value, kind)) = read_clipboard(&mut self.clipboard, self.record_file_paths)
            {
                let changed = self
                    .shared
                    .history
                    .lock()
                    .map(|mut hist| hist.push_kind(value, kind))
                    .unwrap_or(false);
                if changed {
                    self.shared.dirty.store(true, Ordering::SeqCst);
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
        let handler = Handler {
            shared,
            clipboard,
            record_file_paths,
        };
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

/// What the clipboard currently holds, as the text to record and the kind of
/// entry it makes — or `None` when it holds nothing this history can store.
///
/// The file list is asked for first, not as a fallback. It is the flavour that
/// identifies a copy as a file copy, and the only one that is always there:
/// what a file manager puts in the *text* flavour is up to the desktop — GNOME's
/// Files publishes the plain paths, others publish `file://` URIs, and a Windows
/// `CF_HDROP` copy carries no text at all. Text-first would therefore record a
/// URI on one desktop and nothing on another, and even where it happens to
/// yield the right string it lands the entry in the history unlabelled. Asking
/// this way round costs an ordinary text copy one extra round trip, which the
/// clipboard owner refuses outright rather than leaving to time out, on a thread
/// that is only woken by a copy in the first place.
///
/// A file list that yields no paths falls through to the text flavour. That is
/// not a defensive branch: a browser publishes a `text/uri-list` of `https://`
/// URLs when you copy a link, and the decoder keeps only the `file://` ones, so
/// an empty list is the normal way of saying "this is not a file copy".
///
/// With `record_file_paths` off, a copy that *does* name files is dropped
/// instead — see [`FileListOutcome::Ignore`].
fn read_clipboard(
    clipboard: &mut arboard::Clipboard,
    record_file_paths: bool,
) -> Option<(String, EntryKind)> {
    if let Ok(paths) = clipboard.get().file_list() {
        match classify_file_list(&paths, record_file_paths) {
            FileListOutcome::Record(text) => return Some((text, EntryKind::Paths)),
            FileListOutcome::Ignore => return None,
            FileListOutcome::NotAFileCopy => {}
        }
    }
    clipboard
        .get_text()
        .ok()
        .map(|text| (text, EntryKind::Text))
}

/// What a clipboard file list means for the history.
#[derive(Debug, PartialEq, Eq)]
enum FileListOutcome {
    /// A file copy, to be recorded as this text.
    Record(String),
    /// A file copy the user has asked not to record, via `record_file_paths`.
    ///
    /// The copy is dropped rather than falling through to the text flavour the
    /// file manager publishes alongside the files. That flavour is the same copy
    /// by another name — GNOME's Files puts the very same paths there — so
    /// falling through would go on recording every file copy, merely without the
    /// mark saying what it is, and the setting would do nothing at all. The list
    /// is still asked for when the setting is off, because recognising the copy
    /// is the only way to leave it out.
    Ignore,
    /// Not a file copy at all, so the caller should try the text flavour.
    ///
    /// A browser publishes a `text/uri-list` of `https://` URLs when you copy a
    /// link, and the decoder keeps only the `file://` ones — an empty list is
    /// the normal way of saying this, not a malformed clipboard.
    NotAFileCopy,
}

/// Decide what to do with a file list, given the `record_file_paths` setting.
///
/// Split from [`read_clipboard`] because it is the part worth pinning down: the
/// difference between [`Ignore`] and [`NotAFileCopy`] is a deliberate choice
/// rather than an implementation detail, and it is invisible from a call site
/// that has a live clipboard in its hands.
///
/// [`Ignore`]: FileListOutcome::Ignore
/// [`NotAFileCopy`]: FileListOutcome::NotAFileCopy
fn classify_file_list(paths: &[PathBuf], record_file_paths: bool) -> FileListOutcome {
    match paths_to_text(paths) {
        None => FileListOutcome::NotAFileCopy,
        Some(_) if !record_file_paths => FileListOutcome::Ignore,
        Some(text) => FileListOutcome::Record(text),
    }
}

/// Render a clipboard file list as the text to store: one path per line, in the
/// order the file manager listed them. `None` when nothing usable is left.
///
/// A multi-file copy becomes a multi-line entry rather than several entries. It
/// was one action by the user, the popup commits one row at a time, and pasting
/// the lot is what a shell or an editor does something useful with — splitting
/// it would put the pieces on separate rows that can then age out apart.
///
/// The trailing `\r` trim earns its place: `text/uri-list` is defined with CRLF
/// line endings — GNOME's Files does send them — and the decoder these paths
/// come from splits on `\n` alone, so a path arrives with the carriage return
/// still glued to it. Only that one character is trimmed: a filename really can
/// end in a space, and quietly rewriting it would hand the user a path that
/// doesn't resolve.
fn paths_to_text(paths: &[PathBuf]) -> Option<String> {
    let joined = paths
        .iter()
        .map(|path| path.to_string_lossy().trim_end_matches('\r').to_owned())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then_some(joined)
}

struct PopupApp {
    shared: Arc<Shared>,
    selected: usize,
    visible: bool,
    /// False until the first `logic` frame has enforced the hidden state.
    initialized: bool,
    /// Whether the popup has actually held keyboard focus since it was last
    /// shown. Used to defer the focus-loss auto-dismiss until *after* the
    /// window manager has granted focus, so the popup doesn't hide itself on
    /// the very frame it appears (before focus lands).
    focused_once: bool,
    /// Set when the selection moves (or the popup opens); cleared once the
    /// scroll area has followed it. Scrolling to the selection on *every* frame
    /// instead re-centers the list continuously and swallows the mouse wheel,
    /// which is the only way to reach rows far from the selection.
    scroll_to_selection: bool,
}

impl PopupApp {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            selected: 0,
            visible: false,
            initialized: false,
            focused_once: false,
            scroll_to_selection: false,
        }
    }

    fn show_popup(&mut self, ctx: &egui::Context) {
        self.visible = true;
        self.selected = 0;
        self.focused_once = false;
        self.scroll_to_selection = true;
        center_on_screen(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide_popup(&mut self, ctx: &egui::Context) {
        self.visible = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// Commit `chosen`: hide the popup so focus returns to the previously active
    /// window, then (off the UI thread) place the item on the clipboard and
    /// synthesize a paste into that window.
    ///
    /// The item is passed in rather than looked up by index here, because the
    /// store can change between the frame the user saw and the keypress that
    /// commits it: the watcher thread prepends on every clipboard change, which
    /// shifts every index by one, and the tray's Clear empties the list
    /// outright. Resolving `self.selected` against the store at this point would
    /// paste the neighbour of the highlighted item, or nothing at all. The
    /// caller resolves it against the snapshot it actually rendered.
    fn commit_selection(&mut self, ctx: &egui::Context, chosen: Option<Arc<str>>) {
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

    /// Drop the history entry the user clicked the trash icon on, and keep the
    /// highlight on a sensible row.
    ///
    /// `rendered_index` and `rendered_len` describe the snapshot the click
    /// happened on, not the store: the watcher thread can have prepended in the
    /// meantime. They're only used to move the highlight — the entry itself is
    /// identified by its contents (see [`HistoryStore::remove`]), so the wrong
    /// item can't be deleted, at worst the highlight lands a row off.
    fn remove_item(&mut self, rendered_index: usize, item: &str, rendered_len: usize) {
        let removed = self
            .shared
            .history
            .lock()
            .map(|mut h| h.remove(item))
            .unwrap_or(false);
        if !removed {
            return;
        }
        // Straight to disk rather than via `dirty` — see [`flush_history`].
        flush_history(&self.shared);

        self.selected = selection_after_removal(self.selected, rendered_index, rendered_len);
        // Same as every other path that moves the highlight: the new row is
        // usually adjacent to one that was on screen, but deleting near the top
        // of the viewport while the selection sits below the fold would
        // otherwise move it out of view.
        self.scroll_to_selection = true;
    }

    /// Star or unstar the entry the user clicked the star on, keeping the
    /// highlight on whatever entry it was on before.
    ///
    /// Toggling reorders the list — that is the point of the feature — so the
    /// highlight's index means something different afterwards, and leaving it
    /// alone would silently move it to a different entry. `selected` is the
    /// entry it was on in the snapshot that was clicked; the store is asked
    /// where that entry sits now.
    ///
    /// That entry can be gone — a tray "Clear" or the watcher's eviction ran
    /// between the frame that drew the row and this click — and the store can
    /// have got shorter with it. Landing on whatever is at the old index is the
    /// same best effort the delete path makes, but only once the index is back
    /// in range: past the end, no row draws the highlight at all and Enter
    /// commits nothing, so the selection appears to vanish for no visible
    /// reason.
    fn toggle_favorite(&mut self, item: &str, selected: Option<Arc<str>>) {
        let Ok(mut history) = self.shared.history.lock() else {
            return;
        };
        if !history.toggle_favorite(item) {
            return;
        }
        let moved_to = selected.and_then(|text| history.position(&text));
        self.selected = selection_after_toggle(self.selected, moved_to, history.len());
        drop(history);

        // Unlike a delete, this goes through `dirty` rather than straight to
        // disk: a star that doesn't survive a crash in the next few seconds is a
        // lost click, not a secret left behind in `history.json`.
        self.shared.dirty.store(true, Ordering::SeqCst);
        // The entry moved, and often a long way — pinning the last row of a full
        // history puts it at the top, off the visible part of the list.
        self.scroll_to_selection = true;
    }
}

/// Contents of the highlighted row in a rendered snapshot, if there is one.
///
/// The store is never indexed with `self.selected` directly: the watcher thread
/// reorders the history on every clipboard change, so an index resolved against
/// the store rather than the snapshot the user actually clicked can name a
/// different entry (see [`PopupApp::commit_selection`]).
fn selected_text(items: &[history::Entry], selected: usize) -> Option<Arc<str>> {
    items.get(selected).map(|e| e.text.clone())
}

impl eframe::App for PopupApp {
    /// Runs every frame *including while the window is hidden* (unlike `ui`,
    /// which eframe only calls for a visible viewport). The show-on-hotkey
    /// trigger must live here: it sends `Visible(true)`, and only on the next
    /// frame — once the viewport is visible — does `ui` start being called.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Some window managers ignore `ViewportBuilder::with_visible(false)` and
        // map the window anyway, leaving a blank popup on screen at startup.
        // Enforce the hidden state once, up front.
        if !self.initialized {
            self.initialized = true;
            if !self.visible {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }

        if self.shared.show_requested.swap(false, Ordering::SeqCst) {
            self.show_popup(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if !self.visible {
            return;
        }

        // Snapshot the history once per frame so the lock isn't held while we
        // borrow `self` mutably during rendering. The store hands out `Arc<str>`
        // precisely so this stays a few refcount bumps rather than a deep copy
        // of every entry, which at frame rate would be ruinous for the large
        // entries a clipboard routinely holds.
        let items: Vec<history::Entry> = self
            .shared
            .history
            .lock()
            .map(|h| h.snapshot())
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
                    self.scroll_to_selection = true;
                }
                if i.key_pressed(egui::Key::ArrowUp) {
                    self.selected = (self.selected + len - 1) % len;
                    self.scroll_to_selection = true;
                }
            }
        });

        // Esc requested a hide.
        if !self.visible {
            self.hide_popup(&ctx);
            return;
        }
        if commit {
            self.commit_selection(&ctx, selected_text(&items, self.selected));
            return;
        }

        // Dismiss when the popup loses focus (click-away / alt-tab) — but only
        // once it has actually gained focus. Otherwise the popup would hide
        // itself on the first frame after being shown, before the window
        // manager has granted focus to this undecorated window.
        let focused = ctx.input(|i| i.viewport().focused);
        if focused == Some(true) {
            self.focused_once = true;
        }
        if self.focused_once && focused == Some(false) {
            self.hide_popup(&ctx);
            return;
        }

        // The row whose delete button was pressed this frame, as (index in the
        // snapshot, contents). Applied after rendering, so the store isn't
        // mutated while the list built from it is still being drawn.
        let mut to_remove: Option<(usize, Arc<str>)> = None;
        // Likewise for the row whose star was pressed. Only the contents are
        // needed: starring reorders the list, so the rendered index says nothing
        // about where the entry ends up.
        let mut to_toggle_favorite: Option<Arc<str>> = None;

        // --- Render (into the central Ui eframe provides) ---
        // That `Ui` comes with no margin, so without a frame the heading and the
        // rows would sit flush against the edge of this undecorated window. Only
        // the margin is wanted here: a filled frame (`Frame::central_panel`)
        // shrinks to its content, which two-tones the window below the last row.
        egui::Frame::new()
            .inner_margin(POPUP_MARGIN)
            .show(ui, |ui| {
                ui.heading("Clipboard history");
                ui.separator();
                if len == 0 {
                    ui.label("No items yet — copy something.");
                    return;
                }
                // egui's default scroll bar floats *over* the content and swells
                // from 2 to 6 points while hovered, which puts it on top of the
                // delete icons at the right edge. A solid bar allocates its own
                // width instead, so the rows end where it begins.
                ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let row_height = ui.spacing().interact_size.y;
                    // Measured once, outside the loop, and reused for every row.
                    // Reading it per row instead lets any row that overflows widen
                    // the scroll area's content, which widens the next row's
                    // measurement in turn — the delete buttons then walk right
                    // down the list until they fall off the window.
                    let row_width = (ui.available_width() - ROW_TRAILING_GAP).max(0.0);
                    for (idx, item) in items.iter().enumerate() {
                        let selected = idx == self.selected;
                        ui.allocate_ui_with_layout(
                            egui::vec2(row_width, row_height),
                            // Right-to-left: the delete button is placed first and
                            // so keeps the right edge whatever width the glyph and
                            // its padding actually come to. Sizing a column for it
                            // up front and hoping the button fits is what drifted —
                            // `add_sized` is a suggestion, not a clamp.
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                let row_rect = ui.max_rect();
                                // Reserve a slot in the paint order now, and fill it
                                // in once the row's state is known: the highlight has
                                // to land *behind* the preview and the icon, but
                                // whether to draw it depends on a hover that is only
                                // known after they have been added.
                                let highlight = ui.painter().add(egui::Shape::Noop);
                                // Registered before the delete button so that button,
                                // added after and therefore on top, keeps its own
                                // clicks. This response only ever selects or commits.
                                // The id has to be spelled out and unique per row:
                                // `interact` doesn't allocate, so it can't draw one
                                // from the layout the way an added widget does.
                                let row = ui.interact(
                                    row_rect,
                                    ui.id().with(("row", idx)),
                                    egui::Sense::click(),
                                );

                                // `frame_when_inactive(false)` keeps the icon quiet
                                // until it's hovered, so the rows don't read as a
                                // column of buttons.
                                let delete = ui
                                    .add(egui::Button::new(DELETE_ICON).frame_when_inactive(false))
                                    .on_hover_text("Remove from history");
                                if delete.clicked() {
                                    to_remove = Some((idx, item.text.clone()));
                                }

                                // Left of the delete button, for the same reason
                                // it is added after: right-to-left places each
                                // widget at the left edge of what is left.
                                let (star, tooltip) = if item.favorite {
                                    (FAVORITE_ICON_ON, "Remove from favorites")
                                } else {
                                    (FAVORITE_ICON_OFF, "Add to favorites")
                                };
                                // Unframed like the trash icon, so the rows don't
                                // read as a column of buttons; filled vs. outlined
                                // is what carries the state.
                                let favorite = ui
                                    .add(egui::Button::new(star).frame_when_inactive(false))
                                    .on_hover_text(tooltip);
                                if favorite.clicked() {
                                    to_toggle_favorite = Some(item.text.clone());
                                }

                                // Whatever the button left behind, to the pixel.
                                let preview_width = ui.available_width();
                                ui.allocate_ui_with_layout(
                                    egui::vec2(preview_width, row_height),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        // Indent the text off the highlight's left
                                        // edge. The selectable label this replaced
                                        // carried the same padding inside its own
                                        // background; painting the highlight by hand
                                        // means putting it back by hand.
                                        ui.add_space(ui.spacing().button_padding.x);
                                        let selected_color = ui.visuals().selection.stroke.color;
                                        // Ahead of the preview, so the mark reads
                                        // as a property of the row rather than of
                                        // the first path in it. Unframed and
                                        // unselectable: it labels the row, it
                                        // isn't a third button on it.
                                        if item.kind == EntryKind::Paths {
                                            let mut icon = egui::RichText::new(PATH_ICON);
                                            if selected {
                                                icon = icon.color(selected_color);
                                            }
                                            ui.add(egui::Label::new(icon).selectable(false))
                                                .on_hover_text(
                                                    "Copied in the file manager — \
                                                     pastes the path",
                                                );
                                        }
                                        let mut text =
                                            egui::RichText::new(one_line_preview(&item.text));
                                        if selected {
                                            text = text.color(selected_color);
                                        }
                                        // Not a `selectable_label`: the row paints its
                                        // own highlight below, across the icon too, so
                                        // the preview is just text. `selectable(false)`
                                        // keeps it from taking the click as a
                                        // text-selection drag.
                                        ui.add(egui::Label::new(text).truncate().selectable(false));
                                    },
                                );

                                // `contains_pointer` rather than `hovered`: the delete
                                // button sits on top of this rect, and hovering it must
                                // not make the row's highlight blink out.
                                if selected || row.contains_pointer() {
                                    let visuals = ui.visuals();
                                    let fill = if selected {
                                        visuals.selection.bg_fill
                                    } else {
                                        visuals.widgets.hovered.weak_bg_fill
                                    };
                                    ui.painter().set(
                                        highlight,
                                        egui::Shape::rect_filled(
                                            row_rect,
                                            visuals.widgets.hovered.corner_radius,
                                            fill,
                                        ),
                                    );
                                }

                                if row.clicked() {
                                    self.selected = idx;
                                    commit = true;
                                }
                                // Only follow the selection when it actually moved, so
                                // the mouse wheel isn't fighting a re-center every frame.
                                if selected && self.scroll_to_selection {
                                    row.scroll_to_me(Some(egui::Align::Center));
                                }
                            },
                        );
                    }
                });
            });
        self.scroll_to_selection = false;

        // Deleting is handled before committing, and returns rather than falling
        // through. Keep it that way: the two are driven by separate responses over
        // overlapping rectangles, so a click that somehow registered on both would
        // otherwise delete the entry *and* commit — and committing synthesizes a
        // real paste into whatever window regains focus. Losing a click is a
        // non-event; pasting into the user's editor because they aimed at a trash
        // icon is not. The early return is also what keeps the now-stale snapshot
        // from being used: the store is redrawn from scratch next frame.
        if let Some((idx, item)) = to_remove {
            self.remove_item(idx, &item, len);
            return;
        }

        // Same reasoning as the delete above: separate responses over
        // overlapping rectangles, and committing pastes into the user's window.
        if let Some(item) = to_toggle_favorite {
            self.toggle_favorite(&item, selected_text(&items, self.selected));
            return;
        }

        if commit {
            self.commit_selection(&ctx, selected_text(&items, self.selected));
        }
    }
}

/// Where the highlight should land after the row at `removed` is dropped from a
/// list that was `rendered_len` long.
///
/// Everything below the deleted row shifts up by one, so a selection below it
/// follows to stay on the same entry. Deleting the highlighted row itself leaves
/// the index alone, which lands on what was the next entry — except at the end
/// of the list, where there is no next entry and the highlight clamps to the new
/// last row (`rendered_len - 2`). Deleting the only entry leaves nothing to
/// highlight and the clamp saturates to 0.
///
/// Split out of [`PopupApp::remove_item`] to be testable: it's the one piece of
/// the delete path whose off-by-one behaviour isn't obvious by inspection, and
/// getting it wrong shows up only as a highlight on the wrong row.
fn selection_after_removal(selected: usize, removed: usize, rendered_len: usize) -> usize {
    let shifted = if removed < selected {
        selected - 1
    } else {
        selected
    };
    shifted.min(rendered_len.saturating_sub(2))
}

/// Where the highlight should land after a star toggle has reordered a list that
/// is now `len` long.
///
/// `moved_to` is where the entry the highlight was on has ended up, or `None` if
/// it is no longer in the store. Following it is the whole point of resolving it
/// at all: a toggle moves entries past each other, so an index left alone names
/// a different entry afterwards. When the entry is gone — a tray "Clear" or an
/// eviction between the rendered frame and the click — the old index is the best
/// guess left, clamped so a list that got shorter can't leave the highlight past
/// the end of it.
///
/// Split out of [`PopupApp::toggle_favorite`] for the reason
/// [`selection_after_removal`] is split out of the delete path: in the method it
/// sits behind a `Mutex` and an `Arc<Shared>`, so none of these cases are
/// reachable from a test.
fn selection_after_toggle(selected: usize, moved_to: Option<usize>, len: usize) -> usize {
    match moved_to {
        Some(index) => index,
        None => selected.min(len.saturating_sub(1)),
    }
}

/// Collapse a clipboard entry to a single trimmed preview line for the popup.
///
/// Builds the line lazily and stops at [`PREVIEW_MAX_CHARS`]. Flattening the
/// whole entry first would mean walking (and copying) a multi-megabyte clipboard
/// item to keep 80 characters of it, once per visible row per frame.
fn one_line_preview(s: &str) -> String {
    let mut out = String::with_capacity(PREVIEW_MAX_CHARS + 4);
    let mut chars = 0usize;

    for word in s.split_whitespace() {
        let separator = if chars == 0 { "" } else { " " };
        for c in separator.chars().chain(word.chars()) {
            if chars == PREVIEW_MAX_CHARS {
                // More content than fits — mark it and stop reading the rest.
                if out.ends_with(' ') {
                    out.pop();
                }
                out.push('…');
                return out;
            }
            out.push(c);
            chars += 1;
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_copy_is_recorded_when_the_setting_is_on() {
        assert_eq!(
            classify_file_list(&[PathBuf::from("/home/me/notes.md")], true),
            FileListOutcome::Record("/home/me/notes.md".into())
        );
    }

    #[test]
    fn a_file_copy_is_dropped_rather_than_stored_as_a_uri_when_turned_off() {
        // Not `NotAFileCopy`: falling through would reach the text the file
        // manager publishes alongside the files and record the copy anyway,
        // leaving `record_file_paths = false` with nothing to do.
        assert_eq!(
            classify_file_list(&[PathBuf::from("/home/me/notes.md")], false),
            FileListOutcome::Ignore
        );
    }

    #[test]
    fn the_setting_does_not_affect_copies_that_name_no_files() {
        // A browser's `text/uri-list` of `https://` links decodes to nothing.
        // Turning the setting off must not start swallowing ordinary copies.
        assert_eq!(classify_file_list(&[], true), FileListOutcome::NotAFileCopy);
        assert_eq!(
            classify_file_list(&[], false),
            FileListOutcome::NotAFileCopy
        );
    }

    #[test]
    fn a_single_copied_file_becomes_its_path() {
        assert_eq!(
            paths_to_text(&[PathBuf::from("/home/me/notes.md")]).as_deref(),
            Some("/home/me/notes.md")
        );
    }

    #[test]
    fn a_multi_file_copy_becomes_one_entry_per_line() {
        // One action by the user, so one row in the popup — and one paste that a
        // shell or an editor does something useful with.
        assert_eq!(
            paths_to_text(&[PathBuf::from("/home/me/a"), PathBuf::from("/home/me/b")]).as_deref(),
            Some("/home/me/a\n/home/me/b")
        );
    }

    #[test]
    fn a_crlf_uri_list_does_not_leave_a_carriage_return_on_the_path() {
        // `text/uri-list` is defined with CRLF endings and the decoder upstream
        // splits on `\n`, so the `\r` arrives glued to the path. Pasting it
        // would produce a path nothing can open.
        assert_eq!(
            paths_to_text(&[PathBuf::from("/home/me/notes.md\r")]).as_deref(),
            Some("/home/me/notes.md")
        );
    }

    #[test]
    fn a_filename_ending_in_a_space_is_left_alone() {
        // Legal, if unusual. Trimming it would hand back a path that doesn't
        // resolve, which is worse than an odd-looking row.
        assert_eq!(
            paths_to_text(&[PathBuf::from("/home/me/trailing ")]).as_deref(),
            Some("/home/me/trailing ")
        );
    }

    #[test]
    fn a_file_list_with_nothing_in_it_is_not_an_entry() {
        // What a browser's `text/uri-list` of `https://` links decodes to: the
        // caller has to fall through to the text flavour rather than store this.
        assert_eq!(paths_to_text(&[]), None);
        assert_eq!(paths_to_text(&[PathBuf::from("\r")]), None);
    }

    #[test]
    fn deleting_above_the_highlight_follows_it_up() {
        // Rows below the deleted one shift up, so the highlight must too or it
        // lands on the neighbour of the entry the user was looking at.
        assert_eq!(selection_after_removal(3, 1, 5), 2);
        assert_eq!(selection_after_removal(1, 0, 5), 0);
    }

    #[test]
    fn deleting_below_the_highlight_leaves_it_alone() {
        assert_eq!(selection_after_removal(1, 3, 5), 1);
    }

    #[test]
    fn deleting_the_highlighted_row_lands_on_what_was_next() {
        // Same index, one shorter list — that's the row that moved up into it.
        assert_eq!(selection_after_removal(2, 2, 5), 2);
    }

    #[test]
    fn deleting_the_highlighted_last_row_clamps_to_the_new_last() {
        // Nothing moves up into the old index here, so it would point past the
        // end of the shortened list.
        assert_eq!(selection_after_removal(4, 4, 5), 3);
    }

    #[test]
    fn deleting_the_only_entry_does_not_underflow() {
        // An empty list has no row to highlight; the clamp must saturate rather
        // than wrap to usize::MAX.
        assert_eq!(selection_after_removal(0, 0, 1), 0);
    }

    #[test]
    fn starring_the_highlighted_entry_takes_the_highlight_to_the_top() {
        // Driven through the real store rather than hand-computed indices: the
        // point of resolving the entry's new position is that the ordering rules
        // live there, not here.
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        h.push("c".into());
        // ["c", "b", "a"], highlight on "a" — the last row.
        h.toggle_favorite("a");
        assert_eq!(selection_after_toggle(2, h.position("a"), h.len()), 0);
    }

    #[test]
    fn starring_another_row_carries_the_highlight_down_with_its_entry() {
        // "b" keeps its place relative to the rest, but everything below the
        // newly pinned row is one index further down, so a highlight left alone
        // would land on the entry that used to be above it.
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        h.push("c".into());
        // ["c", "b", "a"], highlight on "b".
        h.toggle_favorite("a");
        // ["a", "c", "b"]
        assert_eq!(selection_after_toggle(1, h.position("b"), h.len()), 2);
    }

    #[test]
    fn a_vanished_entry_leaves_the_highlight_where_it_was() {
        // Best effort, same as the delete path: land on whatever is at that
        // index now.
        assert_eq!(selection_after_toggle(1, None, 5), 1);
    }

    #[test]
    fn a_vanished_entry_clamps_into_a_list_that_got_shorter() {
        // A tray "Clear" between the rendered frame and the click. Past the end,
        // no row draws the highlight and Enter commits nothing, so the stale
        // index has to come back into range.
        assert_eq!(selection_after_toggle(7, None, 2), 1);
        // And an empty store must saturate rather than wrap to usize::MAX.
        assert_eq!(selection_after_toggle(7, None, 0), 0);
    }

    #[test]
    fn selected_text_reads_the_rendered_snapshot() {
        let items = vec![
            history::Entry::new("first", false),
            history::Entry::new("second", true),
        ];
        assert_eq!(selected_text(&items, 1).as_deref(), Some("second"));
        // A selection index can outlive the snapshot it was resolved against;
        // out of range must be "nothing chosen", not a panic or a stray entry.
        assert!(selected_text(&items, 2).is_none());
        assert!(selected_text(&[], 0).is_none());
    }

    #[test]
    fn preview_collapses_whitespace() {
        assert_eq!(one_line_preview("  a\n\tb   c  "), "a b c");
        assert_eq!(one_line_preview(""), "");
        assert_eq!(one_line_preview(" \n\t "), "");
    }

    #[test]
    fn preview_keeps_exactly_max_chars_untruncated() {
        let s = "a".repeat(PREVIEW_MAX_CHARS);
        let preview = one_line_preview(&s);
        assert_eq!(preview, s);
        assert!(!preview.ends_with('…'));
    }

    #[test]
    fn preview_truncates_one_char_over_max() {
        let preview = one_line_preview(&"a".repeat(PREVIEW_MAX_CHARS + 1));
        assert_eq!(preview, format!("{}…", "a".repeat(PREVIEW_MAX_CHARS)));
    }

    #[test]
    fn preview_truncates_on_char_boundaries() {
        // Multi-byte input: truncation must count characters, not bytes, and
        // must not split one. (Slicing by byte index here would panic.)
        let preview = one_line_preview(&"é".repeat(PREVIEW_MAX_CHARS * 2));
        assert_eq!(preview.chars().count(), PREVIEW_MAX_CHARS + 1);
        assert!(preview.starts_with(&"é".repeat(PREVIEW_MAX_CHARS)));
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn preview_does_not_leave_a_space_before_the_ellipsis() {
        // Truncating exactly at a word separator shouldn't render "word …".
        let s = format!("{} tail", "a".repeat(PREVIEW_MAX_CHARS - 1));
        let preview = one_line_preview(&s);
        assert_eq!(preview, format!("{}…", "a".repeat(PREVIEW_MAX_CHARS - 1)));
    }

    #[test]
    fn preview_of_a_huge_entry_stays_bounded() {
        let huge = "lorem ipsum ".repeat(500_000); // ~6 MB
        let preview = one_line_preview(&huge);
        assert_eq!(preview.chars().count(), PREVIEW_MAX_CHARS + 1);
    }
}
