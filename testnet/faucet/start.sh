#!/bin/bash
# Start the faucet for this kit's chain on 127.0.0.1:<RPC_PORT + 6>. down.sh stops it.
#   testnet/faucet/start.sh [--home DIR]
# It pays from a wallet of its own, <home>/wallets/faucet, so the operator key never sits behind a
# public endpoint. The first start creates that wallet and its account and prints the account;
# fund it from the operator (the command is printed), then start again.
# Settings: FAUCET_DRIP, FAUCET_PER_DAY, FAUCET_PER_ADDRESS, FAUCET_CLIENT_IP_HEADER (faucet.py).
set -u
# shellcheck source=testnet/lib.sh
. "$(dirname "$0")/../lib.sh"
if [ "${1:-}" = --home ]; then P=$(cd "${2:?--home DIR}" && pwd); shift 2; fi
load_env || exit 1
FW=$P/wallets/faucet
f=$P/run/faucet.pid
if [ -f "$f" ] && kill -0 "$(cat "$f")" 2>/dev/null; then echo "faucet already running, pid $(cat "$f")"; exit 0; fi
fw() { LEE_WALLET_HOME_DIR=$FW LEE_WALLET_PASSWORD=${FAUCET_WALLET_PASSWORD:-} "$WALLET_BIN" "$@"; }
if [ ! -f "$FW/account" ]; then
  umask 077; mkdir -p "$FW"
  "$PY" - "$P/wallets/operator/wallet_config.json" "$FW/wallet_config.json" <<'PY'
import shutil, sys; shutil.copy(sys.argv[1], sys.argv[2])
PY
  fw check-health > "$FW/recovery-phrase.txt" 2>&1 || true
  out=$(fw account new public --label faucet 2>&1)
  id=$(echo "$out" | id_of); pk=$(echo "$out" | pk_of)
  if [ -z "$id" ] || [ -z "$pk" ]; then echo "could not create the faucet account: $out"; exit 1; fi
  echo "$id" > "$FW/account"; echo "$pk" > "$FW/public_key"
  umask 022
fi
FA=$(cat "$FW/account")
bal=$(rpc_call getAccountBalance "[\"$FA\"]" | "$PY" -c 'import json,sys;print(json.load(sys.stdin).get("result") or 0)' 2>/dev/null)
if [ "${bal:-0}" = 0 ]; then
  echo "the faucet account Public/$FA is empty. Fund it from the operator, then start again:"
  K=$KIT; [ "$(pwd)/testnet" = "$KIT" ] && K=testnet
  H=""; [ "$P" != "$KIT/run" ] && H=" --home $P"
  echo "  $K/wallet.sh$H auth-transfer send --from Public/$OP --to-pk $(cat "$FW/public_key") --amount 10000000000"
  exit 1
fi
mkdir -p "$P/run" "$P/logs"
FAUCET_WALLET_BIN=$WALLET_BIN FAUCET_WALLET=$FW FAUCET_ACCOUNT=$FA FAUCET_RPC=$RPC_URL \
FAUCET_LISTEN=127.0.0.1:$((RPC_PORT + 6)) FAUCET_STATE=$P/run/faucet-state.json \
LEE_WALLET_PASSWORD=${FAUCET_WALLET_PASSWORD:-} \
  nohup nice -n 5 "$PY" "$KIT/faucet/faucet.py" >> "$P/logs/faucet.log" 2>&1 < /dev/null &
echo $! > "$f"; sleep 1
if curl -s -m 5 "http://127.0.0.1:$((RPC_PORT + 6))/faucet" >/dev/null; then echo "faucet on 127.0.0.1:$((RPC_PORT + 6)), pid $(cat "$f"), paying from Public/$FA (balance $bal)"
else echo "faucet not answering"; tail -5 "$P/logs/faucet.log"; exit 1; fi
