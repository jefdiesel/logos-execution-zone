#!/bin/bash
# Write testnet/docker/genesis-config.json for docker-compose.yml: upstream's local.json with a
# genesis that funds only the public key you give (64 hex digits, from
# `wallet account get --keys --account-id Public/<id>`), a fresh channel id, 15 s blocks, gossip off.
#   testnet/docker/make-config.sh <public key hex> [block seconds]
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
key=${1:?usage: make-config.sh <public key hex> [block seconds]}
echo "$key" | grep -qE '^[0-9a-fA-F]{64}$' || { echo "the public key must be 64 hex digits" >&2; exit 2; }
python3 - "$HERE/../../lez/sequencer/service/networks/local.json" "$HERE/genesis-config.json" "$key" "${2:-15}" <<'PY'
import json, os, sys
src, dst, key, block = sys.argv[1:5]
c = json.load(open(src))
c["block_create_timeout"] = block + "s"
c["mempool_max_size"] = 200
c["bedrock_config"]["channel_id"] = os.urandom(32).hex()
c["bedrock_config"]["node_url"] = "http://127.0.0.1:9/"
c["genesis"]["actions"] = [{"supply_account": {"recipient": {"identified": {"key": key}}, "balance": 10**14}}]
c["gossip"] = None
c["cross_zone"] = None
json.dump(c, open(dst, "w"), indent=2)
print("wrote", dst)
PY
