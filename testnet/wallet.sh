#!/bin/bash
# The LEZ wallet built by up.sh, pointed at this kit's chain and the operator wallet.
#   testnet/wallet.sh [--home DIR] <wallet arguments>      e.g. testnet/wallet.sh account list
# For another wallet (your own keys), set LEE_WALLET_HOME_DIR yourself and run <home>/bin/wallet.
set -u
. "$(dirname "$0")/lib.sh"
if [ "${1:-}" = --home ]; then P=$(cd "${2:?--home DIR}" && pwd); shift 2; fi
load_env || exit 1
wallet "$@"
