#!/bin/bash
# The filter in front of your running kit chain: the refusals it must make, then the real wallet
# through it (a throwaway account funded by the operator, a transfer and reads), then one client
# hitting the rate limit. Starts the filter if it isn't running (down.sh stops it). Reads and two
# small transfers only; nothing is proved.
#   testnet/proxy/test-live.sh [--home DIR]
set -uo pipefail
# shellcheck source=testnet/lib.sh
. "$(dirname "$0")/../lib.sh"
if [ "${1:-}" = --home ]; then P=$(cd "${2:?--home DIR}" && pwd); shift 2; fi
load_env || exit 1
if [ -z "$(height)" ]; then echo "FAIL  the chain at $RPC_URL is not answering (testnet/up.sh first)"; exit 1; fi
"$KIT/proxy/start.sh" --home "$P" || exit 1
PP=$PROXY_PORT FAILS=0
code() { curl -s -m 20 -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PP/" -H 'Content-Type: application/json' "$@"; }
t() {  # NAME EXPECTED CURL-ARGS...
  local name=$1 want=$2; shift 2; local got; got=$(code "$@")
  if [ "$got" = "$want" ]; then printf 'PASS  %-34s %s\n' "$name" "$got"; else printf 'FAIL  %-34s %s (want %s)\n' "$name" "$got" "$want"; FAILS=$((FAILS + 1)); fi
}
echo "== refusals through the filter on 127.0.0.1:$PP"
t getLastBlockId 200 -d '{"jsonrpc":"2.0","id":1,"method":"getLastBlockId","params":[]}'
t getChannelId 200 -d '{"jsonrpc":"2.0","id":2,"method":"getChannelId","params":[]}'
t requeueCrossZoneDeadLetter 403 -d '{"jsonrpc":"2.0","id":3,"method":"requeueCrossZoneDeadLetter","params":["00"]}'
t getCrossZoneDeadLetters 403 -d '{"jsonrpc":"2.0","id":4,"method":"getCrossZoneDeadLetters","params":[]}'
t rpc.discover 403 -d '{"jsonrpc":"2.0","id":5,"method":"rpc.discover","params":[]}'
t "batch of two" 400 -d '[{"jsonrpc":"2.0","id":6,"method":"getLastBlockId","params":[]},{"jsonrpc":"2.0","id":7,"method":"getLastBlockId","params":[]}]'
t "getBlockRange 0..99 (100)" 200 -d '{"jsonrpc":"2.0","id":8,"method":"getBlockRange","params":[0,99]}'
t "getBlockRange 0..100 (101)" 400 -d '{"jsonrpc":"2.0","id":9,"method":"getBlockRange","params":[0,100]}'
t "WebSocket upgrade" 400 -H 'Upgrade: websocket' -d '{}'
got=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PP/" -H 'Content-Type: text/plain' -d '{}')
if [ "$got" = 415 ]; then echo "PASS  wrong content type                 415"; else echo "FAIL  wrong content type $got"; FAILS=$((FAILS + 1)); fi
got=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PP/")
if [ "$got" = 405 ]; then echo "PASS  GET /                              405"; else echo "FAIL  GET / $got"; FAILS=$((FAILS + 1)); fi
echo "direct to the sequencer for comparison (no filter): batch $(curl -s -o /dev/null -w '%{http_code}' -X POST "$RPC_URL/" -H 'Content-Type: application/json' -d '[{"jsonrpc":"2.0","id":6,"method":"getLastBlockId","params":[]}]'), deadletters $(curl -s -o /dev/null -w '%{http_code}' -X POST "$RPC_URL/" -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":4,"method":"getCrossZoneDeadLetters","params":[]}')"

echo; echo "== the stock wallet through the filter (a throwaway wallet)"
TW=$(mktemp -d "${TMPDIR:-/tmp}/lez-filter-test.XXXXXX"); chmod 700 "$TW"
trap 'rm -rf "$TW"' EXIT
"$PY" - "$P/wallets/operator/wallet_config.json" "$TW/wallet_config.json" "http://127.0.0.1:$PP" <<'PY'
import json, sys
c = json.load(open(sys.argv[1])); c["sequencer"]["sequencer_addr"] = sys.argv[3]
json.dump(c, open(sys.argv[2], "w"), indent=2)
PY
tw() { LEE_WALLET_HOME_DIR=$TW LEE_WALLET_PASSWORD='' "$WALLET_BIN" "$@"; }
tw check-health > "$TW/first-run.txt" 2>&1
if grep -q 'All looks good' "$TW/first-run.txt"; then echo "PASS  wallet check-health through the filter"; else echo "FAIL  check-health: $(grep -v -i phrase "$TW/first-run.txt" | tail -2)"; FAILS=$((FAILS + 1)); fi
out=$(tw account new public 2>&1); X=$(echo "$out" | id_of); XPK=$(echo "$out" | pk_of)
[ -n "$XPK" ] || XPK=$(tw account get --keys --account-id "Public/$X" 2>&1 | pk_of)
wallet auth-transfer send --from "Public/$OP" --to-pk "$XPK" --amount 50000000 > /dev/null 2>&1
for _ in $(seq 1 $((BLOCK_TIME * 4 + 30))); do
  b=$(rpc_call getAccountBalance "[\"$X\"]" | "$PY" -c 'import json,sys;print(json.load(sys.stdin)["result"])' 2>/dev/null)
  [ "${b:-0}" -gt 0 ] && break; sleep 1
done
echo "throwaway Public/$X funded by the operator: $b"
out=$(tw auth-transfer send --from "Public/$X" --to "Public/$OP" --amount 1000 2>&1); rc=$?
if [ "$rc" = 0 ] && echo "$out" | grep -q 'included in block' && ! echo "$out" | grep -q reverted; then echo "PASS  transfer sent through the filter: $(echo "$out" | grep 'included in block')"
else echo "FAIL  transfer through the filter (rc=$rc): $(echo "$out" | tail -3)"; FAILS=$((FAILS + 1)); fi
if tw account get --account-id "Public/$X" 2>&1 | grep -qi balance; then echo "PASS  account read through the filter"; else echo "FAIL  account read"; FAILS=$((FAILS + 1)); fi

echo; echo "== rate limit: one client (CF-Connecting-IP 198.51.100.7), 120 calls back to back"
if "$PY" - "$PP" <<'PY'
import collections, http.client, sys
port = int(sys.argv[1]); c = collections.Counter()
body = b'{"jsonrpc":"2.0","id":1,"method":"getLastBlockId","params":[]}'
for _ in range(120):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    conn.request("POST", "/", body, {"Content-Type": "application/json", "CF-Connecting-IP": "198.51.100.7"})
    r = conn.getresponse(); r.read(); c[r.status] += 1; conn.close()
print("statuses:", dict(c))
sys.exit(0 if c[429] > 0 and c[200] > 0 else 1)
PY
then echo "PASS  rate limit answers 429 past the burst"; else echo "FAIL  no 429"; FAILS=$((FAILS + 1)); fi
echo "healthz: $(curl -s "http://127.0.0.1:$PP/healthz")"
echo; if [ "$FAILS" = 0 ]; then echo "filter: ALL PASS"; else echo "filter: $FAILS FAILED"; fi
[ "$FAILS" = 0 ]
