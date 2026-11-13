#!/bin/bash
# Build what the kit runs from this checkout, and copy the binaries into $P/bin:
#   1. sequencer_service with --features standalone,prove (standalone = no Logos Blockchain node,
#      the mock answers instead; prove = guests run in-process, no external r0vm)
#   2. wallet with --features lee/prove (the wallet simulates every public transaction locally to
#      measure its gas, and proves private ones; with this feature it needs no r0vm either)
#   3. with --examples: the example guest programs (examples/program_deployment) and the
#      run_hello_world runner, for smoke.sh's deploy-and-call step
#
#   build.sh [--examples] [--rebuild]
#
# Cached: a binary is rebuilt only when this checkout's commit or the features changed (or with
# --rebuild). Environment:
#   SEQ_FEATURES      default standalone,prove
#   WALLET_FEATURES   default lee/prove
#   LEZ_BUILD_WRAP    a command prefix for every cargo call, e.g. "nice -n 19"
#   LEZ_BUILD_KEY     the cache key (default: the last commit outside testnet/)
#   CARGO_TARGET_DIR, CARGO_BUILD_JOBS   as cargo reads them
set -uo pipefail
P=${P:?set P to the kit home (up.sh does this)}
SRC=$(cd "$(dirname "$0")/../.." && pwd)
SEQ_FEATURES=${SEQ_FEATURES:-standalone,prove}
WALLET_FEATURES=${WALLET_FEATURES:-lee/prove}
T=${CARGO_TARGET_DIR:-$SRC/target}
EXAMPLES=0; REBUILD=0
for a in "$@"; do
  case $a in --examples) EXAMPLES=1 ;; --rebuild) REBUILD=1 ;; *) echo "unknown argument $a"; exit 2 ;; esac
done
mkdir -p "$P/bin" "$P/logs"
# The cache key is the last commit that touched anything but the kit itself, so a kit-only change
# (scripts, docs) never triggers a rebuild.
COMMIT=$(git -C "$SRC" log -1 --format=%H -- . ':(exclude)testnet' ':(exclude).github' ':(exclude)README.md' 2>/dev/null)
[ -n "$COMMIT" ] || COMMIT=unknown
COMMIT=${LEZ_BUILD_KEY:-$COMMIT}   # CI passes a hash of the sources instead (shallow clones)
DIRTY=$(git -C "$SRC" status --porcelain -- . ':(exclude)testnet' ':(exclude).github' ':(exclude)README.md' 2>/dev/null | head -1)
[ -n "$DIRTY" ] && COMMIT="$COMMIT-dirty"

# build NAME STAMP-KEY OUTPUT-BINARY CARGO-ARGS...
build() {
  local name=$1 key=$2 out=$3; shift 3
  local stamp="$P/bin/.$name.stamp"
  if [ "$REBUILD" = 0 ] && { [ -z "$out" ] || [ -x "$P/bin/$out" ]; } && [ "$(cat "$stamp" 2>/dev/null)" = "$key" ] && [ "${COMMIT%-dirty}" = "$COMMIT" ]; then
    echo "  $name: cached ($P/bin/$out)"; return 0
  fi
  echo "  $name: cargo build $* (log: $P/logs/build-$name.log)"
  local t0; t0=$(date +%s)
  # shellcheck disable=SC2086  # LEZ_BUILD_WRAP is a command prefix, split on purpose
  if ! (cd "$SRC" && ${LEZ_BUILD_WRAP:-} cargo build --release --locked "$@") > "$P/logs/build-$name.log" 2>&1; then
    echo "  $name: BUILD FAILED; last lines of $P/logs/build-$name.log:"; tail -25 "$P/logs/build-$name.log"; return 1
  fi
  [ -n "$out" ] && cp "$T/release/$out" "$P/bin/$out"
  echo "$key" > "$stamp"
  echo "  $name: built in $(( $(date +%s) - t0 )) s"
}

echo "== building from $SRC at ${COMMIT:0:12} (first build: 15-60 min, about 5 GB in $T)"
if [ -n "${SEQ_BIN:-}" ] && [ "$SEQ_BIN" != "$P/bin/sequencer_service" ]; then echo "  sequencer: prebuilt $SEQ_BIN"
else build sequencer "$COMMIT $SEQ_FEATURES" sequencer_service -p sequencer_service --features "$SEQ_FEATURES" || exit 1; fi
if [ -n "${WALLET_BIN:-}" ] && [ "$WALLET_BIN" != "$P/bin/wallet" ]; then echo "  wallet: prebuilt $WALLET_BIN"
else build wallet "$COMMIT $WALLET_FEATURES" wallet -p wallet --features "$WALLET_FEATURES" || exit 1; fi
if [ "$EXAMPLES" = 1 ]; then
  # The guest programs are built by risc0-build with the risc0 Rust toolchain (`rzup install rust`);
  # no Docker is needed.
  build example-guests "$COMMIT" "" -p example_program_deployment_methods || exit 1
  build example-runner "$COMMIT $WALLET_FEATURES" run_hello_world -p program_deployment --bin run_hello_world \
    --features "$WALLET_FEATURES" || exit 1
  elf=$(find "$T/riscv-guest" -path '*example_program_deployment*' -name hello_world.bin 2>/dev/null | head -1)
  if [ -n "$elf" ]; then cp "$elf" "$P/bin/hello_world.bin"
  elif [ ! -f "$P/bin/hello_world.bin" ]; then echo "  examples: hello_world.bin not found under $T/riscv-guest"; exit 1; fi
  echo "  examples: $P/bin/hello_world.bin ($(wc -c < "$P/bin/hello_world.bin" | tr -d ' ') bytes)"
fi
