#!/bin/bash
# Is the kit's chain healthy? Pid, height, block age, listeners (all loopback unless
# --public-bind), home size, the operator's balance, and the proxy/faucet if running.
#   testnet/status.sh [--home DIR]
set -u
. "$(dirname "$0")/lib.sh"
if [ "${1:-}" = --home ]; then P=$(cd "${2:?--home DIR}" && pwd); shift 2; fi
load_env || exit 1
"$KIT/sequencer/chain.sh" status
h1=$(height)
if [ -n "$h1" ]; then
  sleep $((BLOCK_TIME + 2)); h2=$(height)
  if [ -n "$h2" ] && [ "$h2" -gt "$h1" ]; then echo "blocks: producing ($h1 -> $h2 in $((BLOCK_TIME + 2)) s)"
  else echo "blocks: NOT producing (height stayed at $h1 for $((BLOCK_TIME + 2)) s)"; fi
fi
echo "operator Public/$OP balance: $(rpc_call getAccountBalance "[\"$OP\"]" | "$PY" -c 'import json,sys;print(json.load(sys.stdin).get("result"))' 2>/dev/null)"
for s in proxy faucet watch; do
  f=$P/run/$s.pid
  if [ -f "$f" ] && kill -0 "$(cat "$f")" 2>/dev/null; then echo "$s: running, pid $(cat "$f")"; fi
done
if [ -f "$P/run/proxy.pid" ]; then echo "proxy healthz: $(curl -s -m 5 "http://127.0.0.1:$PROXY_PORT/healthz")"; fi
echo "RPC bind: $BIND:$RPC_PORT (metrics 127.0.0.1:$METRICS_PORT)"
