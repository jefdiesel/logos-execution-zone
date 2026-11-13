#!/bin/bash
# Set up, start, stop and check one standalone LEZ 0.4 sequencer out of a kit home.
#   chain.sh setup | start | stop | restart | status
# Normally run by up.sh / down.sh / status.sh, which pass the kit home in P. Settings come from
# $P/kit.env (written by up.sh): ports, bind address, block time, genesis mode.
#
# Layout under P (the same as the public-demo chain this is based on):
#   chain/home            the sequencer home (config, channel signing key, RocksDB)
#   chain/sequencer.pid   pid file; removed by `stop`, so a stopped chain is "stopped on purpose"
#   logs/sequencer.log    the sequencer's output (append only; rotate-logs.sh trims it)
#
# RPC on BIND:RPC_PORT (127.0.0.1 unless up.sh --public-bind), metrics always on
# 127.0.0.1:METRICS_PORT, gossip off (config). No Logos Blockchain node: the standalone build
# mocks it.
set -u
P=${P:?set P to the kit home (up.sh does this)}
# shellcheck source=/dev/null
[ -f "$P/kit.env" ] && . "$P/kit.env"
SEQ_BIN=${SEQ_BIN:-$P/bin/sequencer_service}
NETCFG=${NETCFG:-$(cd "$(dirname "$0")/../.." && pwd)/lez/sequencer/service/networks/local.json}
HOME_DIR=$P/chain/home
PIDF=$P/chain/sequencer.pid
LOG=$P/logs/sequencer.log
BIND=${BIND:-127.0.0.1}
RPC=$BIND:${RPC_PORT:-3040}
RPC_LOCAL=127.0.0.1:${RPC_PORT:-3040}
METRICS=127.0.0.1:${METRICS_PORT:-3041}
BLOCK_TIME=${BLOCK_TIME:-15}
MEMPOOL_MAX=${MEMPOOL_MAX:-200}
GENESIS_BALANCE=${GENESIS_BALANCE:-100000000000000}
MIN_FREE=${MIN_FREE:-12}      # % free memory needed to start
PY=${PYTHON:-python3}

rpc() { curl -s -m 5 -X POST "http://$RPC_LOCAL/" -H 'Content-Type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":[]}"; }
running() { [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; }
free_pct() {  # percent of memory free, or 100 when it cannot be read
  if command -v memory_pressure >/dev/null 2>&1; then
    memory_pressure -Q | awk -F': ' '/percentage/{gsub("%","",$2);print $2+0}'
  elif [ -r /proc/meminfo ]; then
    awk '/MemTotal/{t=$2}/MemAvailable/{a=$2}END{if(t>0)print int(a*100/t); else print 100}' /proc/meminfo
  else echo 100; fi
}
port_busy() {
  if command -v lsof >/dev/null 2>&1; then lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
  else ss -ltn 2>/dev/null | awk '{print $4}' | grep -q ":$1\$"; fi
}
listeners() {  # "proto address" for every socket the pid listens on
  if command -v lsof >/dev/null 2>&1; then lsof -nP -a -p "$1" -i 2>/dev/null | tail -n +2 | awk '{print $8, $9}'
  else ss -ltnup 2>/dev/null | grep "pid=$1," | awk '{print $1, $5}'; fi
}
mkdir -p "$P/logs" "$P/chain"

case "${1:-status}" in
setup)
  [ -e "$HOME_DIR" ] && { echo "$HOME_DIR exists: refusing to re-genesis (reset.sh wipes it)"; exit 1; }
  [ -x "$SEQ_BIN" ] || { echo "no sequencer binary at $SEQ_BIN"; exit 1; }
  # GENESIS: "default" keeps local.json's genesis (LEZ's published test accounts); otherwise OPK
  # is the operator's public key (hex), funded with GENESIS_BALANCE and nothing else.
  if [ "${GENESIS:-new}" != default ]; then
    [ "${#OPK}" = 64 ] || { echo "OPK must be the operator's 64-hex-digit public key"; exit 1; }
  fi
  cfg=$P/chain/genesis-config.json
  "$PY" - "$NETCFG" "$cfg" "${GENESIS:-new}" "${OPK:-}" "$GENESIS_BALANCE" "$METRICS" "$BLOCK_TIME" "$MEMPOOL_MAX" <<'PY' || exit 1
import json, os, sys
src, dst, mode, key, balance, metrics, block_time, mempool = sys.argv[1:9]
c = json.load(open(src))
c["mempool_max_size"] = int(mempool)
c["block_create_timeout"] = block_time + "s"
c["bedrock_config"]["channel_id"] = os.urandom(32).hex()   # a chain of its own
c["bedrock_config"]["node_url"] = "http://127.0.0.1:9/"    # never contacted by the standalone build
if mode != "default":
    c["genesis"]["actions"] = [{"supply_account": {"recipient": {"identified": {"key": key}}, "balance": int(balance)}}]
c["metrics_listen_address"] = metrics
c["gossip"] = None
c["cross_zone"] = None
json.dump(c, open(dst, "w"), indent=2)
PY
  # The Bedrock funding key is unused without a node (standalone); it must be a field element.
  "$SEQ_BIN" setup "$HOME_DIR" --config "$cfg" --funding-public-key "${BEDROCK_KEY:-0101010101010101010101010101010101010101010101010101010101010100}" \
    --create-channel > "$P/logs/setup.log" 2>&1 || { cat "$P/logs/setup.log"; exit 1; }
  chmod 700 "$HOME_DIR"
  echo "setup done: genesis ${GENESIS:-new}, block time ${BLOCK_TIME}s, home $HOME_DIR"
  ;;
start)
  running && { echo "already running, pid $(cat "$PIDF")"; exit 0; }
  [ -f "$HOME_DIR/sequencer_config.json" ] || { echo "no chain at $HOME_DIR: run setup first"; exit 1; }
  for p in "${RPC_PORT:-3040}" "${METRICS_PORT:-3041}"; do port_busy "$p" && { echo "port $p busy"; exit 1; }; done
  f=$(free_pct); [ "$f" -ge "$MIN_FREE" ] || { echo "only ${f}% memory free (need $MIN_FREE): not starting"; exit 1; }
  echo "== start $(date '+%F %T') $SEQ_BIN" >> "$LOG"
  ( ulimit -n 4096 2>/dev/null; RUST_LOG=${RUST_LOG:-warn} exec nohup nice -n 5 "$SEQ_BIN" start "$HOME_DIR" \
      --rpc-listen-address "$RPC" --metrics-listen-address "$METRICS" ) >> "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDF"
  for _ in $(seq 1 90); do rpc getLastBlockId | grep -q result && break; sleep 1; done
  rpc getLastBlockId | grep -q result || { echo "not answering; see $LOG"; tail -5 "$LOG"; exit 1; }
  echo "started pid $(cat "$PIDF"), height $(rpc getLastBlockId)"
  listeners "$(cat "$PIDF")" | awk '{print "  listens", $1, $2}'
  ;;
stop)
  running || { echo "not running"; rm -f "$PIDF"; exit 0; }
  pid=$(cat "$PIDF"); kill -INT "$pid"
  for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
  kill -0 "$pid" 2>/dev/null && { echo "did not stop on SIGINT within 60 s; pid $pid left running"; exit 1; }
  rm -f "$PIDF"; echo "== stop $(date '+%F %T')" >> "$LOG"; echo "stopped"
  ;;
restart) "$0" stop && "$0" start ;;
status)
  if running; then
    pid=$(cat "$PIDF")
    echo "running, pid $pid, rss $(ps -o rss= -p "$pid" | tr -d ' ') KB, open files $( (lsof -p "$pid" 2>/dev/null || ls /proc/"$pid"/fd 2>/dev/null) | wc -l | tr -d ' ')"
    listeners "$pid" | awk '{print "  listens", $1, $2}'
  else echo "not running"; fi
  echo "height: $(rpc getLastBlockId)"
  echo "home: $(du -sh "$HOME_DIR" 2>/dev/null | cut -f1); disk free there: $(df -h "$P" | awk 'NR==2{print $4}'); memory free: $(free_pct)%"
  ;;
*) echo "usage: $0 setup|start|stop|restart|status"; exit 2 ;;
esac
