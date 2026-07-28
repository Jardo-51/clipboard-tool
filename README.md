# clipboard-tool

Light-weight, cross-platform clipboard **history** manager written in Rust.

It runs quietly in the background, records everything you copy, and on a global
hotkey (**Ctrl+Shift+V** by default) pops up a small centered menu of recent
items. Navigate with the **arrow keys**, press **Enter**, and the selected item
is pasted into whatever window you were using.

- **Cross-platform**: Linux (X11 + Wayland), Windows, macOS
- **Resource-efficient**: native binary, no runtime/GC, size-optimized release
  profile, event-driven (idle at rest)
- **Configurable** hotkey, history size, and persistence
- **Starts on login** (optional) and lives in the system tray

## How it works

| Concern | Implementation |
|---|---|
| Clipboard watch/read/write | `clipboard-master` + `arboard` |
| Popup UI | `egui`/`eframe` (one small native window) |
| Global hotkey | `global-hotkey` (X11/Windows/macOS) or the XDG GlobalShortcuts portal via `ashpd` (Wayland) |
| Paste injection | `arboard` (set clipboard) + `enigo` (synthesize Ctrl/Cmd+V) |
| Autostart | `auto-launch` |
| Tray | `tray-icon` |

## Platform support

| Feature | X11 | Wayland (KDE / GNOME 48+) | Wayland (wlroots: Sway/Hyprland) | Windows | macOS |
|---|---|---|---|---|---|
| Global hotkey | ✅ | ✅ portal | ⚠️ use tray "Show history" | ✅ | ✅¹ |
| Clipboard history | ✅ | ✅ | ✅ | ✅ | ✅ |
| Auto-paste | ✅ | ⚠️ manual Ctrl+V² | ✅ virtual keyboard | ✅ | ✅¹ |
| Autostart | ✅ | ✅ | ✅ | ✅ | ✅ |
| Tray icon | ✅³ | ✅³ | ✅³ | ⏳⁴ | ⏳⁴ |

¹ macOS requires granting **Accessibility** permission (System Settings →
Privacy & Security → Accessibility) for the hotkey and paste injection.
² On GNOME/KDE the libei injection path isn't wired yet, so the item is placed
on the clipboard and you press Ctrl+V yourself. wlroots uses the virtual-keyboard
protocol and auto-pastes.
³ Linux tray needs a StatusNotifier host. KDE and most desktops have one;
**GNOME requires the [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/)**.
⁴ Tray on Windows/macOS needs to share the winit event loop and is not wired yet;
the app still runs and the hotkey works.

## Build & run

The toolchain and all native libraries are provided by a Nix flake dev shell —
nothing is installed system-wide.

```bash
# enter the dev shell (first run downloads the toolchain)
nix develop

# run the daemon; then copy a few things and press Ctrl+Shift+V (the default)
cargo run

# tests / lints / formatting
cargo test
cargo clippy
cargo fmt          # --check to verify without rewriting
```

Code is formatted with rustfmt's default style; please run `cargo fmt` before
opening a PR. The bulk-format commit is listed in `.git-blame-ignore-revs`, so
`git config blame.ignoreRevsFile .git-blame-ignore-revs` keeps `git blame`
useful (GitHub applies it automatically).

With [direnv](https://direnv.net/): `direnv allow` auto-enters the shell.

### Install as a package

```bash
# builds a wrapped, runnable binary at ./result/bin/clipboard-tool
nix build

# or install into your profile
nix profile install .

# update after pulling/making code changes
nix profile upgrade clipboard-tool

# remove
nix profile remove clipboard-tool
```

Once installed into your profile, `clipboard-tool` is on your `PATH`
(`~/.nix-profile/bin`) and can be launched from any directory by just typing
`clipboard-tool` — it needs a graphical session (Wayland/X). The wrapped binary
carries its own GL/Vulkan driver environment, so the popup renders outside the
dev shell too.

Without Nix, a standard `cargo build --release` works too, provided the native
dev libraries are present (see `flake.nix` for the exact list: X11/Wayland/GL,
`xdotool`/libxdo, GTK3 + libayatana-appindicator on Linux).

## Autostart

```bash
clipboard-tool --enable-autostart    # start on login
clipboard-tool --disable-autostart
clipboard-tool --autostart-status
clipboard-tool --help
```

This registers the running executable as a login item: a
`~/.config/autostart/*.desktop` entry on Linux, a `Run` registry value on
Windows, and a LaunchAgent on macOS. You can also toggle it from the tray menu.

Packagers: if your package puts a launcher script at the user-facing path (as
the Nix package here does via `wrapProgram`), set `CLIPBOARD_TOOL_EXE` to that
path. Autostart uses it in preference to `current_exe()`, which would otherwise
resolve past the wrapper to the inner binary and start without its environment.

## Configuration

A commented `config.toml` is created on first run in the platform config dir
(`~/.config/clipboard-tool/config.toml` on Linux):

```toml
# global shortcut: "<modifiers>+<Code>". Modifiers: ctrl, shift, alt, super.
# The key is a physical code such as KeyV, KeyA, Digit1, F5.
hotkey = "ctrl+shift+KeyV"

# maximum number of items to remember
history_size = 100

# keep history across restarts (stored as JSON in the data dir)
persist = true
```

- Missing fields fall back to defaults; a malformed file logs a warning and uses
  defaults rather than refusing to start.
- The tray menu and `--help` show whichever hotkey is actually in effect, so a
  customized `hotkey` is reflected there. If the combination can't be registered
  (another application already holds it), the tray drops the hint rather than
  advertising a shortcut that does nothing — open the popup from **Show
  history** instead, or pick a different `hotkey`.
- History is persisted to `~/.local/share/clipboard-tool/history.json` (Linux),
  written atomically (temp file + rename), flushed every few seconds when it
  changes and on quit.

## Tray menu

- **Show history** — open the popup (same as the hotkey)
- **Start on login** — toggle autostart
- **Clear history**
- **Quit**

## Development notes

Project layout:

```
src/
  main.rs        # daemon wiring, session detection, CLI, event loop
  history.rs     # capped, de-duplicating ring buffer
  config.rs      # config.toml load/defaults, hotkey parsing
  persist.rs     # JSON history load/save (atomic)
  ui/…           # (popup lives in main.rs today)
  platform/
    mod.rs       # session detection + backend selection
    inject.rs    # enigo-based paste injection
  tray.rs        # GTK-thread tray (Linux)
  wayland.rs     # GlobalShortcuts portal hotkey (Linux/Wayland)
  autostart.rs   # auto-launch wrapper + CLI
```

The Wayland portal hotkey and the tray require a live desktop session and are
verified by compilation + unit tests; they need on-device testing.
