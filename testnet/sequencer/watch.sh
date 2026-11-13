#!/bin/bash
# Fail-safe watchdog for a kit chain. Run it every 5 minutes (watch-loop below, cron, a systemd
# timer or launchd), or once by hand. Silent when healthy.
#   P=<kit home> watch.sh          (WATCH_DRY=1: say what it would do, do nothing)
#
# It acts only on this kit's own pids (<home>/run/tunnel.pid, proxy.pid, faucet.pid) and on
# chain.sh. Rules, in order:
#   1. free disk where the home lives < DATA_FLOOR_MB -> stop tunnel + chain (a full disk corrupts
#      nothing, but the sequencer panics on failed writes and crash-loops)
#   2. chain died (its pid file is there, the process is not) -> one `chain.sh start` (1 h
#      cooldown); still down after that -> stop the tunnel, so the public sees "down", not a stall.
#      A chain stopped on purpose (`chain.sh stop`, down.sh) has no pid file: left alone.
#   3. height unchanged for FROZEN_STRIKES checks -> one `chain.sh restart` (1 h cooldown); still
#      frozen after that -> stop the tunnel
# A stopped piece stays stopped until a person starts it again.
#
# To run it every 5 minutes without cron (logs rotated hourly):
#   P=<home> nohup bash -c 'n=0; while :; do "$0/watch.sh"; [ $((n % 12)) = 0 ] && "$0/rotate-logs.sh"; n=$((n+1)); sleep 300; done' \
#     "$(pwd)/testnet/sequencer" >> <home>/logs/watch.log 2>&1 & echo $! > <home>/run/watch.pid
set -u
P=${P:?set P to the kit home}
HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=/dev/null
[ -f "$P/kit.env" ] && . "$P/kit.env"
STATE=$P/run/watch.state
RPC=http://127.0.0.1:${RPC_PORT:-3040}/
DATA_FLOOR_MB=${DATA_FLOOR_MB:-2048}
FROZEN_STRIKES=${FROZEN_STRIKES:-3}
DRY=${WATCH_DRY:-0}
log() { echo "$(date '+%F %T') $*"; }
alive() { [ -f "$P/run/$1.pid" ] && kill -0 "$(cat "$P/run/$1.pid")" 2>/dev/null; }
act() { if [ "$DRY" = 1 ]; then log "WOULD $*"; else log "$*"; "$@"; fi; }
stop_pid() { alive "$1" && act kill "$(cat "$P/run/$1.pid")"; }
stop_chain() { act env P="$P" "$HERE/chain.sh" stop; }
chain_alive() { [ -f "$P/chain/sequencer.pid" ] && kill -0 "$(cat "$P/chain/sequencer.pid")" 2>/dev/null; }
height() { curl -s -m 5 -X POST "$RPC" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getLastBlockId","params":[]}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["result"])' 2>/dev/null; }
mkdir -p "$P/run"

last_height=-1 strikes=0 last_kick=0
# shellcheck disable=SC1090
[ -f "$STATE" ] && . "$STATE"
now=$(date +%s)

data_free=$(df -Pm "$P" | awk 'NR==2{print $4}')
if [ "${data_free:-0}" -lt "$DATA_FLOOR_MB" ]; then log "disk ${data_free} MB free < $DATA_FLOOR_MB"; stop_pid tunnel; stop_chain; exit 0; fi

cooled=$(( now - last_kick > 3600 ))
if [ ! -f "$P/chain/sequencer.pid" ]; then
  :   # stopped on purpose (chain.sh stop removes the pid file): nothing to watch
elif ! chain_alive; then
  if [ "$cooled" = 1 ]; then log "chain not running: starting it once"; act env P="$P" "$HERE/chain.sh" start; last_kick=$now
  else log "chain still not running after a start: stopping the tunnel"; stop_pid tunnel; fi
else
  h=$(height)
  if [ -n "$h" ] && [ "$h" != "$last_height" ]; then strikes=0; last_height=$h
  else
    strikes=$((strikes + 1))
    if [ "$strikes" -ge "$FROZEN_STRIKES" ]; then
      if [ "$cooled" = 1 ]; then log "height frozen at ${last_height} for $strikes checks: restarting the chain once"; act env P="$P" "$HERE/chain.sh" restart; last_kick=$now; strikes=0
      else log "height still frozen after a restart: stopping the tunnel"; stop_pid tunnel; fi
    fi
  fi
fi
[ "$DRY" = 1 ] && exit 0
printf 'last_height=%s\nstrikes=%s\nlast_kick=%s\n' "$last_height" "$strikes" "$last_kick" > "$STATE"
