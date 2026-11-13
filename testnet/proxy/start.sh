#!/bin/bash
# Start the RPC filter in front of this kit's chain: 127.0.0.1:<PROXY_PORT> -> 127.0.0.1:<RPC_PORT>.
# Extra arguments go to lez-rpc-proxy.py (e.g. --allow-method NAME, --client-ip-header X-Real-IP,
# --set IP_ALL=10,150). down.sh stops it.
#   testnet/proxy/start.sh [--home DIR] [proxy arguments]
set -u
# shellcheck source=testnet/lib.sh
. "$(dirname "$0")/../lib.sh"
if [ "${1:-}" = --home ]; then P=$(cd "${2:?--home DIR}" && pwd); shift 2; fi
load_env || exit 1
f=$P/run/proxy.pid
if [ -f "$f" ] && kill -0 "$(cat "$f")" 2>/dev/null; then echo "proxy already running, pid $(cat "$f")"; exit 0; fi
mkdir -p "$P/run" "$P/logs"
nohup nice -n 5 "$PY" "$KIT/proxy/lez-rpc-proxy.py" --listen "127.0.0.1:$PROXY_PORT" --upstream "127.0.0.1:$RPC_PORT" "$@" \
  >> "$P/logs/proxy.log" 2>&1 < /dev/null &
echo $! > "$f"; sleep 1
if curl -s -m 5 "http://127.0.0.1:$PROXY_PORT/healthz" >/dev/null; then echo "proxy on 127.0.0.1:$PROXY_PORT -> $RPC_PORT, pid $(cat "$f")"
else echo "proxy not answering"; tail -5 "$P/logs/proxy.log"; exit 1; fi
