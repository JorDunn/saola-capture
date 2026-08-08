#!/bin/bash
# cast-window.sh <window-id> — RecordWindow with the given id, attach the dmabuf consumer,
# then tear the session down. Used to test whether Mutter.ScreenCast's window-id (t)
# is the same id space as niri-ipc's Window.id.
set -u
S="$(cd "$(dirname "$0")" && pwd)"
WID="${1:?window id}"
SC=org.gnome.Mutter.ScreenCast

cleanup() {
  if [ -n "${SESSION:-}" ]; then
    echo "--- Stop session $SESSION"
    busctl --user call $SC "$SESSION" $SC.Session Stop 2>&1 | sed 's/^/    /'
  fi
  [ -n "${MONPID:-}" ] && kill "$MONPID" 2>/dev/null
  echo "--- niri msg casts after teardown:"; niri msg casts 2>&1 | sed 's/^/    /'
}
trap cleanup EXIT

echo "=== CreateSession"
SESSION=$(busctl --user call $SC /org/gnome/Mutter/ScreenCast $SC CreateSession 'a{sv}' 0 \
          | awk '{gsub(/"/,"",$2); print $2}')
echo "    session = $SESSION"

echo "=== RecordWindow window-id=$WID cursor-mode=1"
OUT=$(busctl --user call $SC "$SESSION" $SC.Session RecordWindow 'a{sv}' 2 \
        "window-id" t "$WID" "cursor-mode" u 1 2>&1)
echo "    -> $OUT"
STREAM=$(echo "$OUT" | awk '/^o /{gsub(/"/,"",$2); print $2}')
[ -n "$STREAM" ] || { echo "    RecordWindow failed"; exit 1; }
echo "    stream = $STREAM"

echo "=== Stream Parameters property"
busctl --user get-property $SC "$STREAM" $SC.Stream Parameters 2>&1 | sed 's/^/    /'

busctl --user monitor --match "type='signal',interface='$SC.Stream',member='PipeWireStreamAdded'" \
  > "$S/evidence/.wsa.$$" 2>&1 &
MONPID=$!
sleep 0.5

echo "=== Start"
busctl --user call $SC "$SESSION" $SC.Session Start 2>&1 | sed 's/^/    /'
sleep 1.0
kill $MONPID 2>/dev/null; MONPID=
NODE=$(grep -oP 'UINT32 \K[0-9]+' "$S/evidence/.wsa.$$" | head -1)
rm -f "$S/evidence/.wsa.$$"
echo "    node id = ${NODE:-<none>}"

echo "=== niri msg casts"
niri msg casts 2>&1 | sed 's/^/    /'
[ -n "$NODE" ] || exit 1

echo
echo "=== pwprobe $NODE dmabuf"
"$S/pwprobe" "$NODE" dmabuf
