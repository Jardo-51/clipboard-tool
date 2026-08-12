# clipboard-tool

Light-weight, cross-platform clipboard **history** manager written in Rust.

It runs quietly in the background, records everything you copy, and on a global
hotkey (**Ctrl+Shift+V** by default) pops up a small centered menu of recent
items. Navigate with the **arrow keys**, press **Enter**, and the selected item
is pasted into whatever window you were using. Each row also carries a **star**
and a **trash** icon: the star pins the item to the top of the list, the trash
drops it.

- **Cross-platform**: Linux (X11 + Wayland), Windows, macOS
- **Favorites**: starred items stay pinned above the rolling history and are
  never evicted to make room for new copies
- **File paths**: copying a file or directory in your file manager records its
  path, so you can paste the path as text wherever you need it
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
| File paths from the file manager | ✅ | ✅ | ✅ | ✅ | ✅ |
| Auto-paste | ✅ | ⚠️ manual Ctrl+V² | ✅ virtual keyboard | ✅ | ✅¹ |
| Autostart | ✅ | ✅ | ✅ | ✅ | ✅ |
| Tray icon | ✅³ | ✅³ | ✅³ | ⏳⁴ | ⏳⁴ |

¹ macOS requires granting **Accessibility** permission (System Settings →
Privacy & Security → Accessibility) for the hotkey and paste injection.
² The injection backend is chosen by `enigo` at runtime, not by this app: it is
built with the `wayland` feature on top of the default `x11rb`, and tries the
wlroots virtual-keyboard protocol first, then X11/XTEST. GNOME and KDE implement
neither, and libei — which is what they *do* support — needs enigo's
`libei_tokio` feature and isn't compiled in, so there the item is placed on the
clipboard and you press Ctrl+V yourself.
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

### Which build do I want?

Native dev libraries are required to be present on the machine. You can either:

- use the project's Nix shell, which provides all required libraries
(see `flake.nix` for the exact list: X11/Wayland/GL, `xdotool`/libxdo,
GTK3 + libayatana-appindicator on Linux)

or:

- skip Nix by droping the `nix develop -c` part from the commands below,
in which case you need to ensure manually that you have the libraries installed —
along with a Rust toolchain and `pkg-config`, which the dev shell also supplies.

There are several options to build and run the app:

#### Local development iteration/debugging

Fastest build and run, creates an un-optimized binary suitable for debugging:

```sh
nix develop -c cargo run
```

#### Binary for regular usage

Build an optimized binary for everyday use, build is slower than the first option, but still
takes advantage of the Cargo cache, so it's not terrible:

```sh
nix develop -c cargo build --release
```

When building with Nix, the resulting binary in `target/release/` carries no runtime environment
of its own: started outside the shell, wgpu finds the host's GL/Vulkan drivers instead of nix's
and the popup dies with `FailedToCreateSurfaceForAnyBackend`.

Therefore it either has to be run from the Nix dev shell:

```sh
nix develop -c target/release/clipboard-tool
```

Or you can create a small launcher script, which can be executed from anywhere (recommended):

```sh
#!/bin/sh
# ~/bin/clipboard-tool — fast rebuilds, launchable from anywhere
DIR=/path/to/repository
export CLIPBOARD_TOOL_EXE="$0"
exec nix develop "$DIR" -c "$DIR/target/release/clipboard-tool" "$@"
```

You can put this script in a directory on your `$PATH`.

`nix develop -c` costs ~0.4s at startup, which is irrelevant for a daemon launched
once at login. The `CLIPBOARD_TOOL_EXE="$0"` line matters: without it
`--enable-autostart` writes the bare `target/release` binary into the `.desktop`
entry, and *that* starts with no environment and fails as above (see
[Autostart](#autostart)).

A binary built entirely outside nix, against your own system libraries, runs directly
(doesn't need the Nix shell or launcher script).

Note that `target/release` still uses the size-optimized profile
(`opt-level = "z"`, LTO, one codegen unit), so it is much slower to link than a
debug build — just nowhere near a cold `nix build`.

#### Install as a Nix package

Slowest option to build, but needs nothing beyond Nix itself — no dev shell to enter,
and nix supplies the same native libraries to the sandbox.

**`nix build` is for packaging, not for iterating.** It compiles in a sandbox with
no access to `target/`, and nix caches whole derivations rather than object files,
so *every* source change recompiles everything — all dependencies included, at
`lto = true` / `codegen-units = 1`. There is no "second build is fast". Don't reach
for it just to try out a code change.

The only thing `nix build` gives the binary that cargo doesn't is the **runtime
environment**: `LD_LIBRARY_PATH` plus the Mesa GL/Vulkan variables, which
`wrapProgram` bakes into `result/bin/clipboard-tool`. That is why this one runs from
anywhere, with no dev shell around it, while the binary in `target/` — the same code
without that wrapper — does not.

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

# maximum number of items to remember (favorites are kept on top of this)
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
  changes and on quit. Each entry is stored as `{"text": …, "favorite": …}`,
  plus `"kind": "paths"` on the entries that came from a file-manager copy; a
  file written by an older build — a plain array of strings, or one without the
  `kind` field — still loads, with nothing starred and everything treated as
  ordinary text. A file that can't be parsed at all is moved aside to
  `history.json.corrupt` and reported on stderr, so the next save doesn't
  overwrite the only copy of it.
- Copying a **file or directory** in your file manager records its path. Such a
  copy puts a list of files on the clipboard rather than text, so it used to be
  ignored entirely; now the paths are stored as the entry's text and the row is
  marked with a folder icon. Selecting one pastes the path. Copying several
  files at once gives one entry with one path per line, since it was one action
  and pasting the lot is what a shell or an editor makes use of.
- `history_size` caps the *unstarred* items only. Favorites are exempt, so
  pinning something means it is still there after a busy day of copying; they
  only go away when you unstar them or delete them — even **Clear history
  (except favorites)** leaves them alone.
- Entries larger than **1 MiB** are not recorded, so copying a large file's
  contents doesn't pull tens of megabytes into memory and into `history.json`.
  Such a copy is skipped rather than shortened — a truncated entry would be
  pasted back later as if it were the real thing — and the clipboard itself is
  untouched, so Ctrl+V still pastes it in full.

## Tray menu

- **Show history** — open the popup (same as the hotkey)
- **Start on login** — toggle autostart
- **Clear history (except favorites)** — drops every unstarred entry; starred
  ones stay. To get rid of one of those, unstar it first or delete it from the
  popup.
- **Quit**

## Development notes

Project layout:

```
src/
  main.rs        # daemon wiring, session detection, CLI, event loop, egui popup
  history.rs     # capped, de-duplicating ring buffer with pinned favorites
  config.rs      # config.toml load/defaults, hotkey parsing
  persist.rs     # JSON history load/save (atomic)
  platform/
    mod.rs       # session detection + backend selection
    inject.rs    # enigo-based paste injection
  tray.rs        # GTK-thread tray (Linux)
  wayland.rs     # GlobalShortcuts portal hotkey (Linux/Wayland)
  autostart.rs   # auto-launch wrapper + CLI
```

The Wayland portal hotkey and the tray require a live desktop session and are
verified by compilation + unit tests; they need on-device testing.
