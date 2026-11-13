#!/bin/bash
# Stop everything the kit started from this home: the filter proxy and faucet (if started with
# their pid files in <home>/run), the watchdog loop, then the sequencer (SIGINT, a clean stop).
# The chain's data stays; up.sh starts it again where it stopped.
#   testnet/down.sh [--home DIR]
set -u
. "$(dirname "$0")/lib.sh"
if [ "${1:-}" = --home ]; then P=$(cd "${2:?--home DIR}" && pwd); shift 2; fi
load_env || exit 1
for s in watch faucet proxy; do
  f=$P/run/$s.pid
  if [ -f "$f" ] && kill -0 "$(cat "$f")" 2>/dev/null; then
    pid=$(cat "$f"); kill "$pid"
    for _ in $(seq 1 10); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    echo "$s stopped"
  fi
  rm -f "$f"
done
"$KIT/sequencer/chain.sh" stop
