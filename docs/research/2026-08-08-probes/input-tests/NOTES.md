# Interactive capture-flow validation — 2026-08-08

Live niri 26.04 session, eDP-1: 2560x1600 physical, logical 1706x1066, scale 1.5.
ydotool 1.0.4, ydotoold pid 1233, socket /run/user/1000/.ydotool_socket.

## Test 1 — ydotool smoke: PASS
- `YDOTOOL_SOCKET=/run/user/1000/.ydotool_socket ydotool mousemove -a -x 200 -y 200` -> exit 0
- `ydotool mousemove -x 50 -y 30` (relative) -> exit 0
- Click syntax (1.0.4): `ydotool click 0xC0` full left click; `0x40` = press, `0x80` = release,
  low nibble = button (0x00 left, 0x01 right, 0x02 middle). `-D N` inter-event delay ms.
- `ydotool click` echoes "c0 110" style lines to stdout (code + delay); harmless.

## Test 2 — slurp drag fidelity: PASS (with 2x coordinate scale finding)
Run A: drag injected (200,200)->(600,500) in 4 steps -> slurp printed `400,400 801x601`, exit 0.
Run B: drag injected (100,100)->(400,200) -> slurp printed `200,200 601x201`, exit 0.
- Mapping: logical = injected * 2.0 exactly, both axes, zero offset, linear.
  NOT equal to the 1.5 output scale — it is an artifact of how niri/libinput maps the
  ydotool virtual device's ABS range onto the output. Do not hardcode; calibrate per session
  (one probe drag) or use relative moves. slurp's size is inclusive (+1 px on w/h).
- slurp coordinates are LOGICAL (fit 1706x1066, not 2560x1600).
- Drag events delivered with full fidelity through a grab overlay: press, 4 intermediate
  motions, release all registered. This is exactly what the Stage 6 selection overlay needs.

## Test 3 — screenshot-screen + event stream: PASS
- `niri msg event-stream` (plain text lines; JSON needs `niri msg -j event-stream`).
- `niri msg action screenshot-screen` -> instant, no UI.
- Event: `Screenshot captured: copied to clipboard and saved to /home/jordan/Pictures/Screenshots/Screenshot from 2026-08-08 12-34-50.png`
- File: PNG, 2560x1600 (PHYSICAL pixels), 8-bit RGBA, ~860 KB. Also copied to clipboard.
- Save path pattern: ~/Pictures/Screenshots/Screenshot from YYYY-MM-DD HH-MM-SS.png

## Test 4 — PickColor: PASS
- `busctl --user call org.gnome.Shell.Screenshot /org/gnome/Shell/Screenshot org.gnome.Shell.Screenshot PickColor`
  blocks until click; click at injected (200,200) = logical (400,400) resolved it.
- Returned: `a{sv} 1 "color" (ddd) 0.215686 0.215686 0.215686` = RGB(55,55,55) #373737.
- Verified correct: grim of that logical coordinate immediately after showed #373737 there.
  (A pre-click grim showed #0C0A00 — terminal content scrolled between sample and click;
  the picker's value matched the live pixel.)
- Overlay consumed the click; nothing leaked to apps; busctl exit 0. PickColor is viable.

## Test 5 — niri built-in screenshot UI: PASS
- `niri msg action screenshot` opens selection UI. Escape = `ydotool key 1:1 1:0` closed it.
- UX observed (test5-ui-open.png): screen frozen + dimmed, a default pre-selected bright
  rectangle in the center, bottom hint bar: "Press Space to save the screenshot.
  Press P to hide the pointer."
- IMPORTANT: grim (screencopy) taken WHILE the UI was open captured the UI itself —
  overlay surfaces are composited into screencopy. saola-capture must grab its freeze-frame
  BEFORE mapping its own overlay.
- No niri event emitted for UI open/cancel (stream showed nothing for it).

## Test 6 — session clean: PASS
- Final grim (test6-final-desktop.png): normal desktop, no overlays.
- No leftover slurp / event-stream / busctl processes.

## Misc
- grim `-g "X,Y WxH"` takes LOGICAL coords but outputs PHYSICAL pixels (5x5 logical -> 7x7 px at scale 1.5).
- Foreground timing between injections done via `python3 -c 'import time;time.sleep(n)'`.

Artifacts in this directory: test1-ydotool-smoke.txt, test2-slurp-out.txt, test2b-slurp-out.txt,
test3-event-stream.txt, test4-pickcolor-out.txt, test4-expected-pixel.png, test4-post-region.png,
test5-ui-open.png, test5-ui-closed.png, test6-final-desktop.png (+ -small variants).
