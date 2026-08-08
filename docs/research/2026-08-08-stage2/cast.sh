#!/bin/bash
# cast.sh <mode>  — create a Mutter.ScreenCast monitor cast on eDP-1, attach pwprobe
# in the given mode, then tear the session down unconditionally.
set -u
S="$(cd "$(dirname "$0")" && pwd)"
MODE="${1:-shm}"
SC=org.gnome.Mutter.ScreenCast

cleanup() {
  if [ -n "${SESSION:-}" ]; then
    echo "--- Stop session $SESSION"
    busctl --user call $SC "$SESSION" $SC.Session Stop 2>&1 | sed 's/^/    /'
  fi
  [ -n "${MONPID:-}" ] && kill "$MONPID" 2>/dev/null
  echo "--- niri msg casts after teardown:"
  niri msg casts 2>&1 | sed 's/^/    /'
}
trap cleanup EXIT

echo "=== CreateSession"
SESSION=$(busctl --user call $SC /org/gnome/Mutter/ScreenCast $SC CreateSession 'a{sv}' 0 \
          | awk '{gsub(/"/,"",$2); print $2}')
echo "    session = $SESSION"
[ -n "$SESSION" ] || exit 1

echo "=== RecordMonitor eDP-1 (cursor-mode 1 = embedded)"
STREAM=$(busctl --user call $SC "$SESSION" $SC.Session RecordMonitor 'sa{sv}' "eDP-1" 1 "cursor-mode" u 1 \
         | awk '{gsub(/"/,"",$2); print $2}')
echo "    stream  = $STREAM"
[ -n "$STREAM" ] || exit 1

echo "=== monitoring PipeWireStreamAdded"
busctl --user monitor --match "type='signal',interface='$SC.Stream',member='PipeWireStreamAdded'" \
  > "$S/evidence/.streamadded.$$" 2>&1 &
MONPID=$!
sleep 0.5

echo "=== Start"
busctl --user call $SC "$SESSION" $SC.Session Start 2>&1 | sed 's/^/    /'
sleep 1.0
kill $MONPID 2>/dev/null; MONPID=
NODE=$(grep -A1 "UINT32" "$S/evidence/.streamadded.$$" | grep -oP 'UINT32 \K[0-9]+' | head -1)
[ -z "$NODE" ] && NODE=$(grep -oP 'UINT32 \K[0-9]+' "$S/evidence/.streamadded.$$" | head -1)
echo "    PipeWireStreamAdded node id = ${NODE:-<none>}"
rm -f "$S/evidence/.streamadded.$$"
[ -n "$NODE" ] || exit 1

echo "=== niri msg casts (before consumer attaches)"
niri msg casts 2>&1 | sed 's/^/    /'

echo
echo "=== pwprobe $NODE $MODE"
"$S/pwprobe" "$NODE" "$MODE"
echo

echo "=== niri msg casts (after consumer ran)"
niri msg casts 2>&1 | sed 's/^/    /'
