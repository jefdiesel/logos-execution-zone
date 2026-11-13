# Shared by up.sh, down.sh, status.sh, reset.sh, smoke.sh and wallet.sh. Sourced, not run.
# The kit home P is --home DIR (taken by each script), else $LEZ_TESTNET_HOME, else testnet/run.
# shellcheck shell=bash
KIT=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
P=${LEZ_TESTNET_HOME:-$KIT/run}
PY=${PYTHON:-python3}

# Make P absolute (it may not exist yet).
abs_home() { mkdir -p "$1" && (cd "$1" && pwd); }

load_env() {
  [ -f "$P/kit.env" ] || { echo "no kit home at $P (run testnet/up.sh first, or pass --home DIR)" >&2; return 1; }
  # shellcheck disable=SC1091
  . "$P/kit.env"
  SEQ_BIN=${SEQ_BIN:-$P/bin/sequencer_service}
  WALLET_BIN=${WALLET_BIN:-$P/bin/wallet}
  RPC_URL=http://127.0.0.1:$RPC_PORT
  export P
}

rpc_call() {  # rpc_call METHOD [PARAMS-JSON]
  curl -s -m 10 -X POST "$RPC_URL/" -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}"
}
height() { rpc_call getLastBlockId | "$PY" -c 'import json,sys;print(json.load(sys.stdin)["result"])' 2>/dev/null; }

# The kit's wallet, pointed at this chain. The operator wallet is unencrypted unless you set a
# password (`testnet/wallet.sh set-password`, then export LEE_WALLET_PASSWORD).
wallet() {
  LEE_WALLET_HOME_DIR=$P/wallets/operator LEE_WALLET_PASSWORD=${LEE_WALLET_PASSWORD:-} "$WALLET_BIN" "$@"
}

# Public account id of a public key (hex): base58(SHA256("/LEE/v0.3/AccountId/Public/" | pk)),
# as lee's AccountId::from(&PublicKey). smoke.sh checks it against the wallet.
account_of_pk() {
  "$PY" - "$1" <<'PY'
import hashlib, sys
pk = bytes.fromhex(sys.argv[1])
d = hashlib.sha256(b"/LEE/v0.3/AccountId/Public/\0\0\0\0\0" + pk).digest()
alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
n = int.from_bytes(d, "big"); out = ""
while n:
    n, r = divmod(n, 58); out = alphabet[r] + out
out = "1" * (len(d) - len(d.lstrip(b"\0"))) + out
print(out)
PY
}

# Pull "Public/<id>" and the "pk <hex>" out of wallet output.
id_of() { grep -oE 'Public/[1-9A-HJ-NP-Za-km-z]{32,44}' | head -1 | cut -d/ -f2; }
pk_of() { grep -oiE '(with )?pk [0-9a-f]{64}' | head -1 | awk '{print $NF}'; }

listeners_of() {  # every socket address the pid listens on, one per line
  if command -v lsof >/dev/null 2>&1; then lsof -nP -a -p "$1" -i 2>/dev/null | tail -n +2 | awk '{print $9}'
  else ss -ltnup 2>/dev/null | grep "pid=$1," | awk '{print $5}'; fi
}
