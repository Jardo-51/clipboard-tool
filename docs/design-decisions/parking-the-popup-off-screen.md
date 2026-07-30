# Parking the popup off screen — tried, measured, rejected

**Status:** rejected
**Date:** 2026-07-30
**Context:** [#9 — Popup window flash at app start](https://github.com/Jardo-51/clipboard-tool/issues/9)
**Outcome:** the window is started at one point across instead; see
`POPUP_INITIAL_SIZE` and `PopupApp::logic` in `src/main.rs`.

## The problem this was meant to solve

The popup window appeared on screen for a fraction of a second at launch and
then vanished, despite being asked to start hidden.

`ViewportBuilder::with_visible(false)` only holds until eframe paints. eframe
0.35's `EpiIntegration::post_rendering` calls `window.set_visible(true)` on the
first painted frame unconditionally — on the reasoning that a window should not
be shown before it has something in it — and viewport commands are processed at
the *end* of a frame, after the paint they follow. So the app's `Visible(false)`
cannot get in front of it. The window is going to be put on screen once, and
asking for it back is the only answer available. That map-then-unmap pair is the
flash.

The map cannot be prevented from app code. What it puts on screen can be.

## The idea

Give the window a position far outside the screen in the `ViewportBuilder`, so
eframe's forced appearance happens where nobody can see it, and let
`center_on_screen` — which already runs before every show — bring it back.

It looked like the better of the two options. It needs no extra state, no extra
frames, no resize, and it reuses a code path the popup already depends on. The
window keeps its real size at all times, so nothing can be caught rendering at
the wrong one.

## Why it does not work

A window manager is free to place a window where it likes. The position in
`WM_NORMAL_HINTS` is a hint, not an instruction, and Mutter — GNOME Shell's,
which is the most widely deployed of them — applies its own placement policy to
new windows and overrides what the client asked for.

Measured on a live X session under GNOME Shell 3.36.9 (Mutter), watching root
substructure events with `xev -root -event substructure`, with the viewport
built at `(-1040, -680)` (twice the popup's width and height, to clear the
screen edge with room for a shadow):

```
CreateNotify    window 0x4a00004, (-1040,-680), width 520, height 340
ConfigureNotify window 0x4a00004, (72,27),      width 520, height 340
MapNotify       window 0x4a00004
UnmapNotify     window 0x4a00004
```

The window is created exactly where it was asked to be, and then moved onto the
screen by the window manager *before* it is mapped. The flash happened in plain
view, unchanged.

Note what is and isn't overridden: a position sent later, as a
`ViewportCommand::OuterPosition` on an already-managed window, is honoured —
that is what `center_on_screen` relies on, and it still works. It is only the
initial placement that the window manager claims. And there is no way to get a
command in ahead of the forced map, because commands are processed at the end of
a frame and the map happens during it. The one moment the position needed to
hold is the one moment it does not.

## Two further problems it would have carried

Both were found while implementing it, and both are worth recording because they
apply to any variation on the idea:

- **`center_on_screen` becomes load-bearing in a way it was not.** It sends
  nothing when the monitor size is unknown, which used to mean "leave the window
  where the window manager put it" — a fine answer. With the window parked off
  screen it would mean showing it off the edge, where the user cannot reach it.
  It would need a fallback position it does not currently need.
- **The parked window can lose its monitor.** On X11 this is safe:
  `get_monitor_for_window` falls back to the first monitor when the window
  overlaps none of them, so `monitor_size` stays available. On macOS
  `NSWindow::screen()` returns nil for a fully off-screen window, so
  `current_monitor()` can be `None` — which is exactly the case the fallback
  above would have to cover, on the one platform where it is hardest to test.

## What was done instead

Size, rather than position. A window manager negotiates size with the client
rather than dictating it, so a requested size sticks where a requested position
does not.

The window is born one point across — small enough that its forced appearance
cannot be told from no window at all — and grown to its real size when the first
show is requested. The same `xev` capture after the change shows the window
mapped at 1x1 and never resized while mapped:

```
ConfigureNotify window 0x4c00004, (72,27),  width 1, height 1
ConfigureNotify window 0x4c00004, (110,65), width 1, height 1
MapNotify       window 0x4c00004
UnmapNotify     window 0x4c00004
```

The cost is the state and sequencing in `PopupApp::logic`: the grow has to wait
for a moment the window is known to be off screen, and the show has to wait for
the grow to reach the layout. That is more machinery than the position approach
would have needed. It is also the approach that works.

## When to revisit

If eframe stops forcing the window on screen — the behaviour is
`post_rendering` in `epi_integration.rs`, and a dependency bump is what would
change it — then none of this is needed on any platform, and
`with_visible(false)` can simply be trusted. The startup dance in
`PopupApp::logic` and `POPUP_INITIAL_SIZE` can be deleted together at that
point.
