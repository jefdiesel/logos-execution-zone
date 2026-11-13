#!/bin/bash
# Log rotation for a kit home's logs (<home>/logs), hourly from the watch loop or by hand. Past
# MAX_MB, gzip the last 50k lines to .1.gz (keep .2.gz) and truncate in place. Safe because every
# writer (chain.sh, the proxy, the faucet, the tunnel) appends (`>>`).
#   P=<kit home> rotate-logs.sh     (ROTATE_DRY=1: list what it would rotate)
P=${P:?set P to the kit home}
LOGDIR=${LOGDIR:-$P/logs}
MAX_MB=${MAX_MB:-50}
for f in "$LOGDIR"/*.log; do
  [ -f "$f" ] || continue
  size=$(( $(wc -c < "$f") / 1048576 ))
  [ "$size" -lt "$MAX_MB" ] && continue
  if [ "${ROTATE_DRY:-0}" = 1 ]; then echo "would rotate $f ($size MB)"; continue; fi
  [ -f "$f.1.gz" ] && mv -f "$f.1.gz" "$f.2.gz"
  tail -n 50000 "$f" | gzip > "$f.1.gz"
  : > "$f"
  echo "$(date '+%F %T') rotated $f ($size MB)"
done
