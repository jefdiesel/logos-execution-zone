#!/bin/bash
# Bring up a LEZ 0.4 test network you run yourself: one standalone sequencer on this machine.
#
#   testnet/up.sh [--genesis-key new|default] [--genesis-pubkey HEX] [--port N] [--block-time S]
#                 [--home DIR] [--public-bind] [--examples] [--rebuild]
#
# First run: checks prerequisites, builds the sequencer and the wallet from this checkout
# (cached in <home>/bin), creates the operator wallet and a fresh chain, starts the sequencer in
# the background, waits for the first block and prints how to use it. Later runs: start the
# existing chain again (the genesis flags are fixed at the first run; reset.sh starts over).
#
#   --genesis-key new      (default) a new operator key, created in <home>/wallets/operator;
#                          the genesis funds only that key. Nobody else has it.
#   --genesis-key default  LEZ's published test genesis (lez/sequencer/service/networks/local.json):
#                          its keys are public, so anyone can spend those funds. Local use only.
#   --genesis-pubkey HEX   fund a public key you already hold (64 hex digits). No key is created
#                          here; you sign with your own wallet.
#   --port N               RPC port (default 3040); metrics use N+1, the filter proxy N+5
#   --block-time S         seconds per block (default 15; must be under 25)
#   --home DIR             where the chain, wallets, logs and binaries live (default testnet/run)
#   --public-bind          serve the RPC on 0.0.0.0 instead of 127.0.0.1 (read PUBLIC.md first)
#   --examples             also build the example programs that smoke.sh deploys
#   --rebuild              rebuild the binaries even if cached
#
# Prebuilt binaries instead of building: SEQ_BIN=/path/sequencer_service WALLET_BIN=/path/wallet.
# Build environment: see sequencer/build.sh (LEZ_BUILD_WRAP, CARGO_TARGET_DIR, SEQ_FEATURES).
set -uo pipefail
. "$(dirname "$0")/lib.sh"

GENESIS=""; OPK_ARG=""; PORT=""; BLOCK=""; PUBLIC=0; EXAMPLES=0; REBUILD=0
usage() { sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'; }
while [ $# -gt 0 ]; do
  case $1 in
    --genesis-key) GENESIS=${2:-}; shift 2 ;;
    --genesis-pubkey) GENESIS=pubkey; OPK_ARG=${2:-}; shift 2 ;;
    --port) PORT=${2:-}; shift 2 ;;
    --block-time) BLOCK=${2:-}; shift 2 ;;
    --home) P=${2:-}; shift 2 ;;
    --public-bind) PUBLIC=1; shift ;;
    --examples) EXAMPLES=1; shift ;;
    --rebuild) REBUILD=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
  esac
done
case ${GENESIS:-new} in new|default|pubkey) ;; *) echo "--genesis-key is new or default" >&2; exit 2 ;; esac
[ -z "$OPK_ARG" ] || echo "$OPK_ARG" | grep -qE '^[0-9a-fA-F]{64}$' || { echo "--genesis-pubkey needs 64 hex digits" >&2; exit 2; }
[ -z "$PORT" ] || { [ "$PORT" -ge 1024 ] 2>/dev/null && [ "$PORT" -le 65530 ]; } || { echo "--port must be 1024-65530" >&2; exit 2; }
[ -z "$BLOCK" ] || { [ "$BLOCK" -ge 1 ] 2>/dev/null && [ "$BLOCK" -lt 25 ]; } || { echo "--block-time must be 1-24 seconds (under the channel's posting timeout of 25 s)" >&2; exit 2; }

# ---- prerequisites, with the exact fix for each --------------------------------------------
missing=0
need() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1    install: $2"; missing=1; }; }
OS=$(uname -s)
need curl "your package manager (apt install curl / brew install curl)"
need git "your package manager (apt install git / xcode-select --install)"
need "$PY" "apt install python3 / xcode-select --install (macOS ships python3 with the Command Line Tools)"
if [ -z "${SEQ_BIN:-}" ] || [ -z "${WALLET_BIN:-}" ] || [ "$EXAMPLES" = 1 ]; then
  need cargo "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   (rust-toolchain.toml then installs the pinned toolchain)"
  need pkg-config "apt install pkg-config / brew install pkg-config"
  need clang "apt install build-essential clang libclang-dev / xcode-select --install"
  if [ "$OS" = Linux ]; then
    pkg-config --exists libpcsclite 2>/dev/null || { echo "missing: libpcsclite (the wallet's Keycard support links it)    install: sudo apt install libpcsclite-dev"; missing=1; }
    pkg-config --exists openssl 2>/dev/null || { echo "missing: OpenSSL headers    install: sudo apt install libssl-dev"; missing=1; }
  fi
fi
if [ "$EXAMPLES" = 1 ]; then
  { command -v rzup >/dev/null 2>&1 || [ -x "$HOME/.risc0/bin/rzup" ]; } || {
    echo "missing: rzup (builds the example guest programs)    install: curl -L https://risczero.com/install | bash && rzup install rust && rzup install cargo-risczero 3.0.5"; missing=1; }
fi
[ "$missing" = 0 ] || exit 1

P=$(abs_home "$P"); export P
chmod 700 "$P"
mkdir -p "$P/bin" "$P/logs" "$P/chain" "$P/run"

# ---- settings: fixed at the first run ------------------------------------------------------
if [ -f "$P/kit.env" ]; then
  # shellcheck source=/dev/null
  . "$P/kit.env"
  for pair in "GENESIS:$GENESIS" "RPC_PORT:$PORT" "BLOCK_TIME:$BLOCK"; do
    k=${pair%%:*}; v=${pair#*:}
    if [ -n "$v" ] && [ "$v" != "$(eval echo "\${$k}")" ]; then
      echo "this home was set up with $k=$(eval echo "\${$k}"); to change it: testnet/reset.sh --home $P, then up.sh again" >&2; exit 2
    fi
  done
  if [ "$PUBLIC" = 1 ] && [ "$BIND" != 0.0.0.0 ]; then
    sed -i.bak 's/^BIND=.*/BIND=0.0.0.0/' "$P/kit.env" && rm -f "$P/kit.env.bak"; BIND=0.0.0.0
  fi
else
  RPC_PORT=${PORT:-3040}
  BIND=127.0.0.1; [ "$PUBLIC" = 1 ] && BIND=0.0.0.0
  cat > "$P/kit.env" <<EOF
# Written by up.sh on $(date '+%F %T'). Read by every kit script. Change only via reset.sh + up.sh.
GENESIS=${GENESIS:-new}
RPC_PORT=$RPC_PORT
METRICS_PORT=$((RPC_PORT + 1))
PROXY_PORT=$((RPC_PORT + 5))
BIND=$BIND
BLOCK_TIME=${BLOCK:-15}
EOF
  # shellcheck source=/dev/null
  . "$P/kit.env"
fi
[ -n "${SEQ_BIN:-}" ] && ! grep -q '^SEQ_BIN=' "$P/kit.env" && echo "SEQ_BIN=$SEQ_BIN" >> "$P/kit.env"
[ -n "${WALLET_BIN:-}" ] && ! grep -q '^WALLET_BIN=' "$P/kit.env" && echo "WALLET_BIN=$WALLET_BIN" >> "$P/kit.env"
load_env || exit 1
if [ "$BIND" = 0.0.0.0 ]; then
  cat >&2 <<'EOF'
WARNING: --public-bind serves the sequencer's RPC on every interface. That RPC has no auth, no
rate limit and accepts 10 MiB bodies and batches. Anyone who can reach this port can fill your
mempool and disk. Put it behind the filter proxy and a firewall instead (testnet/PUBLIC.md).
EOF
fi

# ---- build ------------------------------------------------------------------------------------
B=(); [ "$EXAMPLES" = 1 ] && B+=(--examples); [ "$REBUILD" = 1 ] && B+=(--rebuild)
export SEQ_BIN WALLET_BIN   # build.sh skips a binary given from outside the kit home
if [ "$SEQ_BIN" != "$P/bin/sequencer_service" ] && [ "$WALLET_BIN" != "$P/bin/wallet" ] && [ "$EXAMPLES" = 0 ]; then
  echo "== using prebuilt binaries: $SEQ_BIN, $WALLET_BIN (no build)"
else
  "$KIT/sequencer/build.sh" "${B[@]+"${B[@]}"}" || exit 1
fi
for b in "$SEQ_BIN" "$WALLET_BIN"; do [ -x "$b" ] || { echo "not executable: $b" >&2; exit 1; }; done

# ---- operator wallet + genesis (first run only) --------------------------------------------
W=$P/wallets/operator
if [ ! -f "$P/chain/home/sequencer_config.json" ]; then
  umask 077
  mkdir -p "$W"; chmod 700 "$P/wallets" "$W"
  if [ ! -f "$W/wallet_config.json" ]; then
    "$PY" - "$KIT/../lez/wallet/configs/debug/wallet_config.json" "$W/wallet_config.json" "$RPC_URL" <<'PY'
import json, sys
c = json.load(open(sys.argv[1])); c["sequencer"]["sequencer_addr"] = sys.argv[3]
json.dump(c, open(sys.argv[2], "w"), indent=2)
PY
  fi
  if [ ! -f "$W/storage.json" ]; then
    # The first wallet command creates the storage and prints its recovery phrase: that goes to a
    # mode-600 file, never to the terminal or a log.
    wallet check-health > "$W/recovery-phrase.txt" 2>&1 || true
    [ -f "$W/storage.json" ] || { echo "wallet setup failed:"; grep -v -i 'phrase\|^  ' "$W/recovery-phrase.txt"; exit 1; }
  fi
  case $GENESIS in
    new)
      if ! grep -q '^OP=' "$P/kit.env"; then
        out=$(wallet account new public --label operator 2>&1)
        OP=$(echo "$out" | id_of); OPK=$(echo "$out" | pk_of)
        [ -n "$OP" ] && [ "${#OPK}" = 64 ] || { echo "could not create the operator account:"; echo "$out"; exit 1; }
        printf 'OP=%s\nOPK=%s\n' "$OP" "$OPK" >> "$P/kit.env"
      fi ;;
    default)
      if ! grep -q '^OP=' "$P/kit.env"; then
        # PRIVATE_KEY_PUB_ACC_A in lez/testnet_initial_state/src/lib.rs: published in the LEZ repo.
        out=$(wallet account import public --private-key 10a26a9aec7d34b82364eeae45c5294dbb0a764b000b94eeb9b58511dc487c4d 2>&1)
        OP=$(echo "$out" | id_of)
        [ -n "$OP" ] || { echo "could not import the test account:"; echo "$out"; exit 1; }
        wallet account label --account-id "Public/$OP" --label operator >/dev/null 2>&1 || true
        printf 'OP=%s\nOPK=\n' "$OP" >> "$P/kit.env"
      fi ;;
    pubkey)
      grep -q '^OP=' "$P/kit.env" || printf 'OP=%s\nOPK=%s\n' "$(account_of_pk "$OPK_ARG")" "$OPK_ARG" >> "$P/kit.env" ;;
  esac
  umask 022
  load_env || exit 1
  OPK=$OPK "$KIT/sequencer/chain.sh" setup || exit 1
fi

# ---- start, wait for a block -----------------------------------------------------------------
"$KIT/sequencer/chain.sh" start || exit 1
h0=$(height); deadline=$(( $(date +%s) + BLOCK_TIME * 3 + 30 ))
while :; do
  h=$(height)
  [ -n "$h" ] && [ -n "$h0" ] && [ "$h" -gt "$h0" ] && break
  [ "$(date +%s)" -gt "$deadline" ] && { echo "no new block within $((BLOCK_TIME * 3 + 30)) s; see $P/logs/sequencer.log"; exit 1; }
  sleep 1
done
bal=$(rpc_call getAccountBalance "[\"$OP\"]" | "$PY" -c 'import json,sys;print(json.load(sys.stdin).get("result"))' 2>/dev/null)
# Print commands the way the user will type them: relative to the checkout when run from its root.
K=$KIT; [ "$(pwd)/testnet" = "$KIT" ] && K=testnet
H=""; [ "$P" != "$KIT/run" ] && H=" --home $P"
cat <<EOF

LEZ 0.4 test network is up (a chain you run yourself; not the Logos testnet).
  RPC          $RPC_URL  (bound to $BIND:$RPC_PORT)
  height       $h, a block every ${BLOCK_TIME}s
  operator     Public/$OP  balance $bal  (genesis: $GENESIS)
  home         $P
EOF
case $GENESIS in
  new) echo "  operator key in $W (mode 700; back up that folder and recovery-phrase.txt in it)" ;;
  default) echo "  the operator key is LEZ's published test key: anyone can spend these funds" ;;
  pubkey) echo "  the operator key is yours; it is not in this machine's kit wallet" ;;
esac
cat <<EOF

Try it:
  $K/wallet.sh$H account get --account-id Public/$OP
  $K/wallet.sh$H account new public --label alice      # prints alice's account id and "pk <hex>"
  $K/wallet.sh$H auth-transfer send --from Public/$OP --to-pk <alice's pk> --amount 1000000
  curl -s -X POST $RPC_URL/ -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getLastBlockId","params":[]}'
Status: $K/status.sh$H   Stop: $K/down.sh$H   Check everything: $K/smoke.sh$H
EOF
