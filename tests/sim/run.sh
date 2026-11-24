#!/bin/bash
# Two vocsend processes talking through simulated air, in real time.
#
#   tests/sim/run.sh "<rx arguments>" "<tx arguments>" [file for tx's stdin] [noise] [late]
#
#   tests/sim/run.sh "" "README.md"                        # one-way file
#   tests/sim/run.sh "negotiate" "negotiate README.md"     # negotiated
#   tests/sim/run.sh "negotiate" "negotiate LICENSE" "" 0.06 late
#
# Each side gets a stand-in speaker and microphone (play.py, rec.py). What one
# plays, the other hears, together with a quieter copy of its own sound and
# background noise (0.003 by default; 0.06 is a poor link). `late` starts the
# receiver after the sender. Needs python3 with numpy and a release build.
# The receiver's files land in $WORK/rx; logs in $WORK.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
bin="$here/../../target/release/vocsend"
WORK="${WORK:-$(mktemp -d)}"
mkdir -p "$WORK/a" "$WORK/b" "$WORK/rx"
for side in a b; do
    printf '#!/bin/sh\nexec python3 %s/play.py\n' "$here" > "$WORK/$side/aplay"
    printf '#!/bin/sh\nexec python3 %s/rec.py\n' "$here" > "$WORK/$side/arecord"
    chmod +x "$WORK/$side/aplay" "$WORK/$side/arecord"
done
: > "$WORK/tape-a"; : > "$WORK/tape-b"
# Use the stand-in speaker and microphone even on a machine with real ones.
export AIR_T0="$(date +%s.%N)" AIR_NOISE="${4:-0.003}" VOCSEND_AUDIO=commands
start="$PWD"
receiver() { (cd "$WORK/rx" && AIR_OUT="$WORK/tape-b" AIR_IN="$WORK/tape-a" PATH="$WORK/b:/usr/bin:/bin" timeout "${TMO:-300}" "$bin" rx $1 > "$WORK/rx.out" 2> "$WORK/rx.log"; echo "rx exit $?" >> "$WORK/rx.log"); }
sender() { (cd "$start" && AIR_OUT="$WORK/tape-a" AIR_IN="$WORK/tape-b" PATH="$WORK/a:/usr/bin:/bin" timeout "${TMO:-300}" "$bin" tx $1 < "${2:-/dev/null}" > /dev/null 2> "$WORK/tx.log"; echo "tx exit $?" >> "$WORK/tx.log"); }
if [ "${5:-}" = late ]; then sender "$2" "${3:-}" & sleep 6; receiver "$1" & else receiver "$1" & sleep 2; sender "$2" "${3:-}" & fi
wait
echo "--- sender"; cat "$WORK/tx.log"
echo "--- receiver"; cat "$WORK/rx.log"
echo "--- receiver's files and output are in $WORK"
