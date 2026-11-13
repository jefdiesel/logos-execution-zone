#!/bin/bash
# End-to-end check of a running kit chain (start it with up.sh first). Exits nonzero on any FAIL.
#   testnet/smoke.sh [--home DIR] [--no-deploy]
#
#  1. listeners: the sequencer listens on loopback only (unless up.sh --public-bind)
#  2. blocks: the height grows
#  3. account id: the kit's pk -> account id formula agrees with the wallet
#  4. fund: the operator funds a new account by its public key (--to-pk)
#  5. transfer: that account pays a second new account (--to-pk), which pays back (--to: a used
#     account), and every balance adds up to the fees the chain reports
#  6. deploy at a chosen header: `wallet program-loader deploy` of the example hello_world guest at
#     a header account the wallet holds the key of
#  7. call: the example runner deploys hello_world again and calls it; the account's state must
#     read "Hola mundo!"
# Steps 6-7 need the example programs: `testnet/up.sh --examples` (or sequencer/build.sh
# --examples) builds them; --no-deploy skips them (reported as SKIP, never as a pass).
set -uo pipefail
. "$(dirname "$0")/lib.sh"
DEPLOY=1
while [ $# -gt 0 ]; do
  case $1 in
    --home) P=$(cd "${2:?--home DIR}" && pwd); shift 2 ;;
    --no-deploy) DEPLOY=0; shift ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
load_env || exit 1
HELLO_ELF=${HELLO_ELF:-$P/bin/hello_world.bin}
HELLO_RUNNER=${HELLO_RUNNER:-$P/bin/run_hello_world}
FAILS=0; T0=$(date +%s)
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; FAILS=$((FAILS + 1)); }
skip() { echo "SKIP  $*"; }
step() { echo; echo "== $* ($(( $(date +%s) - T0 )) s)"; }
balance() { rpc_call getAccountBalance "[\"$1\"]" | "$PY" -c 'import json,sys;print(json.load(sys.stdin)["result"])' 2>/dev/null; }
wait_balance() {  # ACCOUNT PREDICATE-ON-$b SECONDS: poll until the balance satisfies it
  local b; local end=$(( $(date +%s) + $3 ))
  while [ "$(date +%s)" -le "$end" ]; do
    b=$(balance "$1"); if [ -n "$b" ] && "$PY" -c "import sys;b=int(sys.argv[1]);sys.exit(0 if ($2) else 1)" "$b"; then echo "$b"; return 0; fi
    sleep 2
  done
  echo "${b:-}"; return 1
}
new_account() {  # LABEL -> "ID PK"
  local out; out=$(wallet account new public --label "$1" 2>&1)
  local id; id=$(echo "$out" | id_of)
  [ -n "$id" ] || { echo "account new failed: $out" >&2; return 1; }
  local pk; pk=$(echo "$out" | pk_of)
  [ -n "$pk" ] || pk=$(wallet account get --keys --account-id "Public/$id" 2>&1 | pk_of)
  echo "$id $pk"
}
TAG=$(date +%H%M%S)

step "1. listeners"
pid=$(cat "$P/chain/sequencer.pid" 2>/dev/null)
if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then fail "sequencer not running (testnet/up.sh)"; exit 1; fi
addrs=$(listeners_of "$pid" | sort -u | tr '\n' ' ')
echo "sequencer pid $pid listens on: $addrs"
bad=$(listeners_of "$pid" | grep -vE '^(127\.0\.0\.1|\[::1\]|localhost):' | grep -v "^0\.0\.0\.0:$RPC_PORT\$" || true)
if [ "$BIND" != 0.0.0.0 ] && listeners_of "$pid" | grep -q "^0\.0\.0\.0:$RPC_PORT\$"; then bad="$bad 0.0.0.0:$RPC_PORT"; fi
if [ -z "$bad" ]; then pass "every listener is loopback${BIND:+ (RPC bind $BIND)}"; else fail "non-loopback listeners: $bad"; fi

step "2. blocks"
h1=$(height); sleep $((BLOCK_TIME * 2 + 2)); h2=$(height)
if [ -n "$h1" ] && [ -n "$h2" ] && [ "$h2" -gt "$h1" ]; then pass "height $h1 -> $h2 in $((BLOCK_TIME * 2 + 2)) s"; else fail "height did not grow: '$h1' -> '$h2'"; fi

step "3. account id formula"
if [ "$GENESIS" = default ]; then
  # LEZ's published genesis key A (lez/sequencer/service/networks/local.json) and its account.
  calc=$(account_of_pk 6aa4c365699b89d963ef85a59868b9612b6001f90750b4970662a62776332e81)
else
  calc=$(account_of_pk "$OPK")
fi
if [ "$calc" = "$OP" ]; then pass "account_of_pk(operator pk) = Public/$OP"; else fail "formula gives $calc, wallet says $OP"; fi

step "4. fund a new account by its public key"
opbal=$(balance "$OP"); echo "operator balance $opbal"
read -r A APK <<< "$(new_account "smoke-a-$TAG")" || true
if [ -z "${A:-}" ] || [ -z "${APK:-}" ]; then fail "could not create account A"; exit 1; fi
echo "A = Public/$A (pk $APK)"
FUND=100000000
out=$(wallet auth-transfer send --from "Public/$OP" --to-pk "$APK" --amount "$FUND" 2>&1); rc=$?
echo "$out" | grep -v "^Transaction data" | tail -3
if b=$(wait_balance "$A" "b == $FUND" $((BLOCK_TIME * 4 + 30))); then pass "A holds $b (send rc=$rc)"; else fail "A holds '${b}', expected $FUND (send rc=$rc)"; fi

step "5. transfers between users"
read -r B BPK <<< "$(new_account "smoke-b-$TAG")" || true
echo "B = Public/$B"
out=$(wallet auth-transfer send --from "Public/$A" --to-pk "$BPK" --amount 1000000 2>&1); rc=$?
echo "$out" | grep -v "^Transaction data" | tail -3
if b=$(wait_balance "$B" "b == 1000000" $((BLOCK_TIME * 4 + 30))); then pass "A -> B (--to-pk, never-used account): B holds $b"; else fail "B holds '$b' (send rc=$rc)"; fi
a_mid=$(balance "$A"); echo "A after paying 1000000 + fee: $a_mid (fee $((FUND - 1000000 - a_mid)))"
out=$(wallet auth-transfer send --from "Public/$B" --to "Public/$A" --amount 400000 2>&1); rc=$?
echo "$out" | grep -v "^Transaction data" | tail -3
if b=$(wait_balance "$A" "b == $a_mid + 400000" $((BLOCK_TIME * 4 + 30))); then pass "B -> A (--to, used account): A holds $b"; else fail "A holds '$b', expected $((a_mid + 400000)) (send rc=$rc)"; fi
bb=$(balance "$B"); echo "B after paying 400000 + fee: $bb (fee $((1000000 - 400000 - bb)))"

if [ "$DEPLOY" = 0 ]; then
  step "6-7. deploy and call"; skip "--no-deploy"
elif [ ! -f "$HELLO_ELF" ] || [ ! -x "$HELLO_RUNNER" ]; then
  step "6-7. deploy and call"; fail "no example programs at $HELLO_ELF / $HELLO_RUNNER: run testnet/up.sh --examples (needs rzup), or pass --no-deploy"
else
  step "6. deploy hello_world at a chosen header"
  size=$(wc -c < "$HELLO_ELF" | tr -d ' '); nseg=$(( (size + 98303) / 98304 ))   # MAX_SEGMENT_DATA_LEN = 96 KiB
  read -r H _ <<< "$(new_account "smoke-header-$TAG")"
  segs=()
  for i in $(seq 1 "$nseg"); do read -r s _ <<< "$(new_account "smoke-seg$i-$TAG")"; segs+=("Public/$s"); done
  echo "header Public/$H, $nseg segment(s) for $size bytes"
  out=$(wallet program-loader deploy --elf "$HELLO_ELF" --header "Public/$H" --segments "${segs[@]}" --payer "Public/$OP" 2>&1); rc=$?
  echo "$out" | grep -v "^Transaction data" | tail -4
  acct=$(rpc_call getAccount "[\"$H\"]")
  if [ "$rc" = 0 ] && echo "$acct" | grep -q '"result":{'; then pass "program deployed at header Public/$H"; else fail "deploy rc=$rc; header account: $(echo "$acct" | cut -c1-200)"; fi

  step "7. deploy + call through the example runner"
  read -r C _ <<< "$(new_account "smoke-hello-$TAG")"
  out=$(LEE_WALLET_HOME_DIR=$P/wallets/operator LEE_WALLET_PASSWORD=${LEE_WALLET_PASSWORD:-} "$HELLO_RUNNER" "$HELLO_ELF" "$C" "$OP" 2>&1); rc=$?
  echo "$out" | grep -v "^Transaction data" | tail -3
  end=$(( $(date +%s) + BLOCK_TIME * 4 + 30 )); got=""
  while [ "$(date +%s)" -le "$end" ]; do
    got=$(wallet account get --raw --scope all --account-id "Public/$C" 2>&1)
    echo "$got" | grep -qi '486f6c61206d756e646f21' && break
    sleep 3
  done
  if echo "$got" | grep -qi '486f6c61206d756e646f21'; then pass "Public/$C's hello_world state reads \"Hola mundo!\" (runner rc=$rc)"
  else fail "no greeting in Public/$C's state (runner rc=$rc): $(echo "$got" | tail -3)"; fi
fi

echo
if [ "$FAILS" = 0 ]; then echo "smoke: ALL PASS in $(( $(date +%s) - T0 )) s, height $(height)"; else echo "smoke: $FAILS FAILED"; fi
[ "$FAILS" = 0 ]
