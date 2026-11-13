#!/bin/bash
# Start the chain over: stop it and delete its data (every block, balance and deployed program).
# The operator wallet and the settings are kept, so the next up.sh makes a new genesis that funds
# the same operator key. Public account ids stay the same; anything a wallet stored about private
# accounts on the old chain is stale.
#   testnet/reset.sh [--home DIR] [--keep-old] [--all] [--yes]
#     --keep-old   move the old chain to chain/home.old-<time> instead of deleting it
#     --all        delete the whole home: wallets and keys too (asks first unless --yes)
set -u
. "$(dirname "$0")/lib.sh"
KEEP=0; ALL=0; YES=0
while [ $# -gt 0 ]; do
  case $1 in
    --home) P=$(cd "${2:?--home DIR}" && pwd); shift 2 ;;
    --keep-old) KEEP=1; shift ;; --all) ALL=1; shift ;; --yes) YES=1; shift ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
load_env || exit 1
"$KIT/down.sh" --home "$P" || exit 1
if [ "$ALL" = 1 ]; then
  if [ "$YES" != 1 ]; then
    printf 'Delete %s entirely, including the operator wallet and its keys? Type yes: ' "$P"
    read -r answer; [ "$answer" = yes ] || { echo "nothing deleted"; exit 1; }
  fi
  rm -rf "$P"; echo "deleted $P"; exit 0
fi
if [ -d "$P/chain/home" ]; then
  if [ "$KEEP" = 1 ]; then old=$P/chain/home.old-$(date +%Y%m%d-%H%M%S); mv "$P/chain/home" "$old"; echo "old chain kept at $old"
  else rm -rf "$P/chain/home"; echo "chain data deleted"; fi
fi
rm -f "$P/chain/genesis-config.json"
echo "next: testnet/up.sh${P:+ --home $P} makes a new genesis for operator Public/$OP"
