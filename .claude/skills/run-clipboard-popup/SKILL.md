---
name: run-clipboard-popup
description: Launch clipboard-tool and drive its egui popup on a real X display — build in the nix dev shell, seed a throwaway history, trigger the hotkey, screenshot the window. Use when asked to run or start this app, screenshot the popup, or confirm a UI change looks right in the real app rather than in tests.
user-invocable: true
allowed-tools:
  - Bash
  - Read
  - Write
---

# Run the clipboard-tool popup

Launches the app, makes the popup appear, and captures it as a PNG you can
actually look at. Verified on a Linux host with a live X session.

## Rules that are not optional

1. **Everything runs under `nix develop -c`.** Launching the binary directly
   dies with `Wgpu(CreateSurfaceError(... FailedToCreateSurfaceForAnyBackend))`.
   The dev shell is what supplies mesa, `LIBGL_DRIVERS_PATH`, and
   `VK_DRIVER_FILES` (lavapipe as the software fallback) — see `flake.nix`.
2. **Never send Enter to the popup.** Enter runs `commit_selection`, which puts
   the entry on the real clipboard and synthesizes a real paste into whichever
   window regains focus. On a live desktop that types into the user's editor or
   browser. Dismiss with **Escape**.
3. **Sandbox the XDG dirs.** Without this the app reads and rewrites the user's
   real clipboard history at `~/.local/share/clipboard-tool/history.json` —
   which holds whatever passwords and tokens they have copied.

## Step 1 — Seed a throwaway history

The popup shows nothing until the clipboard watcher records something, so
pre-seed the store instead of waiting on a real copy. The file is a plain JSON
array of strings, newest first (`src/persist.rs`).

```bash
mkdir -p /tmp/ct-run/data/clipboard-tool /tmp/ct-run/config
cat > /tmp/ct-run/data/clipboard-tool/history.json <<'EOF'
["https://github.com/example/repo/pull/1",
 "cargo build --release --locked",
 "A long entry that runs past the eighty-character preview limit so the truncation and the ellipsis are visible in the screenshot",
 "multi\nline\nsnippet with a tab\there",
 "ff4a242"]
EOF
```

Include a long entry and a multi-line one — they exercise `one_line_preview`,
which is where layout regressions show up first.

## Step 2 — Build and launch

`cargo run` builds and launches in one `nix develop` entry. Entering the shell
costs ~1s warm and ~3s when nix's eval cache is cold, so a separate
`cargo build` step just pays that twice for nothing.

Pick the display from `$DISPLAY`, or `ls /tmp/.X11-unix/` if it is unset.
Blanking `WAYLAND_DISPLAY` forces `detect_session()` down the X11 path, where
the `global-hotkey` key grab works and `xdotool` can trigger it; the Wayland
portal path cannot be driven this way.

```bash
DISPLAY=:2 WAYLAND_DISPLAY= \
  XDG_DATA_HOME=/tmp/ct-run/data XDG_CONFIG_HOME=/tmp/ct-run/config \
  nix develop -c cargo run > /tmp/ct-run/app.log 2>&1 &
for i in $(seq 120); do pgrep -x clipboard-tool >/dev/null && break; sleep 0.5; done
```

Poll for the process rather than sleeping a fixed number of seconds. The
compile now happens inside that wait, so its length varies from nothing to
minutes on a cold `target/`, and a blind sleep would send the hotkey into a
process that does not exist yet — which then reads like a failed key grab.

If the loop times out, the build failed: the error is in `/tmp/ct-run/app.log`,
since `cargo run` reports compile errors there rather than at the launch
command. `Gtk-Message: Failed to load module "canberra-gtk-module"` is benign.

Once the process is up, no window appears yet — the viewport starts hidden by
design.

## Step 3 — Show the popup and find its window

The default hotkey is `Ctrl+Shift+V` (`DEFAULT_HOTKEY` in `src/config.rs`). The
app holds a global grab on it, so the keystroke does not reach whatever window
has focus.

```bash
DISPLAY=:2 xdotool key --clearmodifiers ctrl+shift+v
sleep 2
DISPLAY=:2 wmctrl -lG | awk '$NF=="clipboard-tool"'   # -> id, and 460x340 geometry
WIN=$(DISPLAY=:2 wmctrl -lG | awk '$NF=="clipboard-tool"{print $1}')
```

Match the title exactly. A plain `grep clipboard-tool` also hits any terminal
whose title carries the repo path, and you would screenshot that instead.

If no window is listed, the grab failed (another clipboard manager already owns
the shortcut — `register_global_hotkey` warns and degrades rather than
aborting). Check `/tmp/ct-run/app.log`.

## Step 4 — Screenshot the window, and look at it

Capture the window by id, not the root: the root grabs the user's whole desktop
into the transcript.

```bash
DISPLAY=:2 xwd -id "$WIN" -out /tmp/ct-run/popup.xwd
ffmpeg -y -loglevel error -i /tmp/ct-run/popup.xwd /tmp/ct-run/popup.png
```

`Read` the PNG. A blank or uniformly black frame means it never rendered — that
is a failure, not a pass. Things worth checking deliberately: padding at all
four edges, the selection highlight spanning the full row width, long entries
truncating with an ellipsis, and a **uniform background** (a horizontal seam
below the last row means a filled `Frame` shrank to its content instead of
covering the window).

## Step 5 — Drive it

```bash
DISPLAY=:2 xdotool key --clearmodifiers Down Down   # selection moves + follows
DISPLAY=:2 xdotool key --clearmodifiers Escape      # dismiss (never Enter)
```

Re-screenshot after the arrows to confirm the highlight tracked the selection.
After Escape the window disappears from `wmctrl -l` but the process stays alive,
ready for the next hotkey.

## Step 6 — Clean up

```bash
pkill -x clipboard-tool
rm -rf /tmp/ct-run
```

Match the process name with `-x`, not `pkill -f target/debug/clipboard-tool`:
the `-f` pattern also matches the shell running the `pkill`, so the command
kills itself (exit 144) and every step after it silently never runs.
