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
2. **Never send Enter to the popup, and never click a row anywhere but its star
   or trash button.** Both run `commit_selection`, which puts the entry on the
   real clipboard and synthesizes a real paste into whichever window regains
   focus. On a live desktop that types into the user's editor or browser. The
   whole row commits, not just the preview text: `ui.interact(row_rect, …)` in
   `main.rs` senses clicks across the row's full width, so the empty space
   between the end of a short preview and the icons commits too — and that gap is
   exactly where a near-miss aimed at an icon lands. Dismiss with **Escape**. The
   per-row star and trash buttons are safe to click — they only edit the
   throwaway history — but they sit a few pixels from the rest of the row, which
   commits, so read "Driving the mouse" below before aiming at them.
3. **Sandbox the XDG dirs.** Without this the app reads and rewrites the user's
   real clipboard history at `~/.local/share/clipboard-tool/history.json` —
   which holds whatever passwords and tokens they have copied.

## Step 1 — Seed a throwaway history

The popup shows nothing until the clipboard watcher records something, so
pre-seed the store instead of waiting on a real copy. The file is a JSON array
of `{"text": …, "favorite": …}` objects, favorites first and newest first within
each block (`src/persist.rs`, `src/history.rs`). A bare string is also accepted
for backwards compatibility and means "not a favorite", so an older seed file
still works.

```bash
mkdir -p /tmp/ct-run/data/clipboard-tool /tmp/ct-run/config
cat > /tmp/ct-run/data/clipboard-tool/history.json <<'EOF'
[{"text": "git rebase -i origin/main", "favorite": true},
 {"text": "https://github.com/example/repo/pull/1", "favorite": false},
 {"text": "cargo build --release --locked", "favorite": false},
 {"text": "A long entry that runs past the eighty-character preview limit so the truncation and the ellipsis are visible in the screenshot", "favorite": false},
 {"text": "multi\nline\nsnippet with a tab\there", "favorite": false},
 {"text": "ff4a242", "favorite": false}]
EOF
```

Include a long entry and a multi-line one — they exercise `one_line_preview`,
which is where layout regressions show up first — and at least one favorite, so
the filled star and the pinned-to-top ordering are both visible.

## Step 2 — Check nothing else is already running

```bash
pgrep -ax clipboard-tool   # must print nothing
```

Only one process can hold the X11 grab on the hotkey. A second instance starts
fine, logs `hotkey: could not register 'ctrl+shift+KeyV' (HotKey already
registered)` and degrades to no hotkey at all — so `xdotool` presses go to the
*other* process, your popup never appears, and the run looks broken for reasons
that have nothing to do with the change under test. It also poisons the
readiness poll in the next step, which would match the other process and return
instantly.

If something is running, it may be the user's own daemon or a leftover from an
earlier run of this skill. `pkill -x clipboard-tool` when it is yours; ask
before killing one you did not start. To coexist with a daemon you must not
touch, give the sandboxed config a different shortcut instead — the format is
`"<modifiers>+<Code>"` with winit key codes (`src/config.rs`):

```bash
mkdir -p /tmp/ct-run/config/clipboard-tool
printf 'hotkey = "ctrl+shift+F9"\n' > /tmp/ct-run/config/clipboard-tool/config.toml
```

## Step 3 — Build and launch

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
for i in $(seq 600); do pgrep -x clipboard-tool >/dev/null && break; sleep 0.5; done
```

Poll for the process rather than sleeping a fixed number of seconds. The
compile now happens inside that wait, so its length varies from nothing to
minutes on a cold `target/`, and a blind sleep would send the hotkey into a
process that does not exist yet — which then reads like a failed key grab. The
cap is five minutes for the same reason: a cold build has to fit inside it, or
the poll gives up on a compile that is still going fine.

If the loop times out, either the build failed or it is still running — check
`/tmp/ct-run/app.log`, where `cargo run` reports compile errors and progress
rather than at the launch command. An error there means the build failed; a log
that ends mid-compile means it just needs longer, so wait rather than debugging
a launch that has not happened yet. `Gtk-Message: Failed to load module
"canberra-gtk-module"` is benign.

Once the process is up, no window appears yet — the viewport starts hidden by
design.

## Step 4 — Show the popup and find its window

The default hotkey is `Ctrl+Shift+V` (`DEFAULT_HOTKEY` in `src/config.rs`). The
app holds a global grab on it, so the keystroke does not reach whatever window
has focus.

```bash
DISPLAY=:2 xdotool key --clearmodifiers ctrl+shift+v
sleep 2
DISPLAY=:2 wmctrl -lG | awk '$NF=="clipboard-tool"'   # -> id, and 520x340 geometry
WIN=$(DISPLAY=:2 wmctrl -lG | awk '$NF=="clipboard-tool"{print $1}')
```

Match the title exactly. A plain `grep clipboard-tool` also hits any terminal
whose title carries the repo path, and you would screenshot that instead.

If no window is listed, start with the log — the grab is the usual culprit and
it always says so:

```bash
grep -i hotkey /tmp/ct-run/app.log
```

`could not register` means something else owns the shortcut (go back to step 2;
it may also be a clipboard manager the desktop ships). `register_global_hotkey`
warns and degrades rather than aborting, so the process is still alive and
looks healthy — the absence of a window is the only other symptom. Do not
conclude the popup is broken, or that the WM refuses to map it, until that
grep comes back empty: the window exists as an unmapped 520x340 client from the
moment the app starts, so `xwininfo` reporting `IsUnMapped` proves nothing on
its own.

## Step 5 — Screenshot the window, and look at it

Capture the window by id, not the root: the root grabs the user's whole desktop
into the transcript.

```bash
DISPLAY=:2 xwd -id "$WIN" -out /tmp/ct-run/popup.xwd
ffmpeg -y -loglevel error -i /tmp/ct-run/popup.xwd /tmp/ct-run/popup.png
```

`Read` the PNG. A blank or uniformly black frame means it never rendered — that
is a failure, not a pass. Things worth checking deliberately: padding at all
four edges, the selection highlight spanning the full row width, long entries
truncating with an ellipsis, the star and trash icons lining up in a column at
the right edge (filled star on the seeded favorite, which must be the first row;
outlined on the others), and a **uniform background** (a horizontal seam below
the last row means a filled `Frame` shrank to its content instead of covering the
window).

## Step 6 — Drive it

```bash
DISPLAY=:2 xdotool key --clearmodifiers Down Down   # selection moves + follows
DISPLAY=:2 xdotool key --clearmodifiers Escape      # dismiss (never Enter)
```

Re-screenshot after the arrows to confirm the highlight tracked the selection.
After Escape the window disappears from `wmctrl -l` but the process stays alive,
ready for the next hotkey.

### Driving the mouse

Take the popup's origin from `xwininfo`, never from `wmctrl`:

```bash
DISPLAY=:2 xwininfo -id "$WIN" | grep -E "Absolute|Width|Height"
```

`wmctrl -lG` reports desktop-scaled coordinates — on a HiDPI session they come
back doubled (220,130 for a window actually at +110+65), and clicks aimed with
them land in whatever window is behind the popup. The screenshot PNG is 1:1
with the window, so screen position = `xwininfo` origin + the pixel position
you read off the image.

Hover before you click, and confirm the hit in a screenshot:

```bash
DISPLAY=:2 xdotool mousemove $X $Y; sleep 1        # then screenshot
DISPLAY=:2 xdotool getmouselocation --shell        # WINDOW= must be $(printf '%d' $WIN)
DISPLAY=:2 xdotool click 1
```

Each icon button draws a frame and a tooltip under the pointer — "Remove from
history" for the trash, "Add to favorites"/"Remove from favorites" for the star
just left of it — so the screenshot tells you which button you are on, or whether
you are on the row around them; anywhere on that row other than a button commits,
pasting into the user's desktop (rule 2). Do not treat a near-miss as harmless: a click that
lands a few pixels short of the icon is still inside the row, so it pastes. Only
a click that misses the popup entirely is inert — that one focuses another
window and the popup auto-dismisses on focus loss, which is correct behaviour,
not a bug to chase.

## Step 7 — Clean up

```bash
pkill -x clipboard-tool
rm -rf /tmp/ct-run
```

Match the process name with `-x`, not `pkill -f target/debug/clipboard-tool`:
the `-f` pattern also matches the shell running the `pkill`, so the command
kills itself (exit 144) and every step after it silently never runs.
