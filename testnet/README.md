# A LEZ 0.4 test network you run yourself

This directory turns a checkout of the Logos Execution Zone (LEZ) release candidate into a small
test network on your own machine: one command builds it, starts a chain, and gives you a funded
account. You can then send transactions, deploy programs and, if you want, let other people use
it.

**What it is:** one LEZ v0.4 sequencer in `standalone` mode, built from this branch (upstream tag
`v0.4.0-rc3` plus the patches in [PATCHES.md](PATCHES.md)), with the stock wallet.

**What it is not:** it is not the Logos testnet and not run by Logos. The public Logos testnet
still runs LEZ 0.3, and there is no public 0.4 testnet yet. When an official 0.4 testnet exists,
use that instead; this kit is for trying 0.4 before then. A kit chain has a single sequencer
(you), no anchoring to the Logos Blockchain, and no finality beyond your own disk
([PUBLIC.md](PUBLIC.md) lists what that means).

This branch tracks upstream release candidates: it is the upstream tag plus a short series of
fixes, each also prepared as a pull request to upstream. [SYNC.md](SYNC.md) is how it moves to a
new tag.

## Requirements

| | |
|---|---|
| OS | Linux (x86-64 or arm64) or macOS (Apple Silicon). Tested: macOS 15 arm64, Ubuntu 24.04 x86-64 (CI) |
| Rust | `rustup` ([rustup.rs](https://rustup.rs)). `rust-toolchain.toml` pins 1.98.1; cargo installs it on first use |
| System packages, Ubuntu/Debian | `sudo apt install build-essential clang libclang-dev libssl-dev pkg-config libpcsclite-dev curl git python3` (`libpcsclite-dev`: the wallet links PC/SC for Keycard support) |
| System packages, Fedora | `sudo dnf install clang clang-devel openssl-devel pkgconf pcsc-lite-devel` |
| System packages, macOS | `xcode-select --install` and `brew install pkg-config openssl` |
| risc0 (only for `--examples`) | `curl -L https://risczero.com/install \| bash`, then `rzup install rust` and `rzup install cargo-risczero 3.0.5`. This builds the example guest programs with risc0's Rust toolchain; no Docker. The kit's binaries don't need `r0vm` (see Build features) |
| First build | about 5 GB under `target/` (4.2 GB measured on macOS, examples included) plus about 1 GB of downloaded crates and the toolchains; 8 GB of RAM or more; 15-60 minutes (measured: about 40-55 min on a GitHub Ubuntu runner, about 40 min on an Apple M4 at `-j 4`) |
| Running | about 200 MB of RAM, a few MB of disk at first |

## Quick start

```sh
git clone --branch testnet-0.4 https://github.com/jefdiesel/logos-execution-zone.git
cd logos-execution-zone
testnet/up.sh --examples      # build (once), create the chain, start it, print a funded account
testnet/smoke.sh              # end-to-end check: blocks, funding, transfers, a program deploy
```

`up.sh` ends with something like this (a full run is in [EXAMPLE-RUN.txt](EXAMPLE-RUN.txt)):

```
LEZ 0.4 test network is up (a chain you run yourself; not the Logos testnet).
  RPC          http://127.0.0.1:3040  (bound to 127.0.0.1:3040)
  height       2, a block every 15s
  operator     Public/<id>  balance 100000000000000  (genesis: new)
```

and three commands to try. `testnet/wallet.sh` is the stock LEZ wallet pointed at your chain and
the operator wallet; any `wallet` command works through it.

| Command | Does |
|---|---|
| `testnet/up.sh` | build if needed, start (first run: create) the chain; flags below |
| `testnet/status.sh` | pid, height, whether blocks are coming, listeners, disk, operator balance |
| `testnet/down.sh` | stop the chain (and the filter, faucet, watchdog if running); data stays |
| `testnet/reset.sh` | delete the chain's data; the next `up.sh` makes a new genesis for the same operator |
| `testnet/smoke.sh` | check everything end to end; exits nonzero on any failure |
| `testnet/wallet.sh ...` | the wallet, on this chain |

`up.sh` flags: `--genesis-key new|default`, `--genesis-pubkey HEX`, `--port N` (RPC; metrics N+1,
filter N+5, faucet N+6), `--block-time S` (1-24, default 15), `--home DIR` (default
`testnet/run`), `--public-bind`, `--examples`, `--rebuild`. Every script takes `--home DIR`.
Prebuilt binaries: `SEQ_BIN=... WALLET_BIN=... testnet/up.sh`.

## Funding: the genesis key

With `new` or `--genesis-pubkey`, the genesis block gives the whole supply (10^14 units) to one
operator account. Which key that is:

- `--genesis-key new` (default): `up.sh` creates a new wallet in `<home>/wallets/operator`
  (mode 700) and a new key in it. The wallet's recovery phrase goes to
  `<home>/wallets/operator/recovery-phrase.txt` (mode 600), never to the screen or a log. Only
  you have this key. Back up that folder.
- `--genesis-pubkey HEX`: fund a key you already hold (a public key, 64 hex digits, from
  `wallet account get --keys --account-id Public/<id>` in your own wallet). No key material is
  created or copied here. Use this to re-genesis a chain for the same operator you had before.
- `--genesis-key default`: LEZ's published test genesis from
  `lez/sequencer/service/networks/local.json`. Its private keys are in the LEZ repository
  (`lez/testnet_initial_state`), so **everyone who uses this setting shares them** and anyone can
  spend those funds. It funds two public accounts (10^13 and 2x10^13) and two private ones; the
  kit imports the first public one as the operator. Fine on your laptop; never for a chain other
  people can reach.

Each chain also gets a fresh random channel id, so two kit chains are never the same chain.

## Wallet basics

```sh
W=testnet/wallet.sh
$W account list
$W account new public --label alice                 # prints Public/<id> and its public key
$W account get --keys --account-id Public/<alice>   # the public key again ("pk <hex>")
$W account get --account-id Public/<alice>          # balance and state
$W auth-transfer send --from Public/<operator> --to-pk <alice's public key> --amount 1000000
$W auth-transfer send --from Public/<alice> --to Public/<bob> --amount 1000   # bob used before
```

- **A never-used account needs `--to-pk`.** LEZ 0.4 credits a public account that has never been
  used only with its public key as evidence ("admission"). Sending `--to Public/<id>` works when
  the account is in your own wallet (the wallet adds the key itself) or has been used before. For
  someone else's new account, the rc3 wallet simulates the transfer, sees it would revert
  ("declared public but not admitted"), and refuses to send it; forced through with
  `--allow-revert` it is included, reverted, and charged its whole gas limit (16,004,088 units
  at base fees of 8, the wallet's quote). Ask the recipient for their public key and use `--to-pk`.
- **Fees.** Every public transaction pays execution gas and storage gas at base fees that start
  at 8 and rise when blocks are full. The sender must hold the fee *reserve* (gas limit x
  execution base fee + bytes x storage base fee) before it runs, not just the fee, or it is
  refused at admission; a zero-balance account can't send anything. On rc3 a transaction that
  reverts is charged its whole gas limit. `$W --dry-run auth-transfer send ...` simulates a
  transaction and prints its gas, fee and reserve without sending it.
- **Private accounts and shielded transfers** (`account new private`, `auth-transfer send` to or
  from a `Private/` account): the standalone sequencer verifies private transactions against the
  privacy circuit like any other sequencer, so they are expected to work, but **neither smoke.sh
  nor the run in EXAMPLE-RUN.txt sends one**. The proof is made on your machine, in the wallet
  process, with every core: expect minutes per transaction and several GB of RAM (a large private
  transaction took about 11 minutes and 9.4 GB on an Apple M4, on an rc1 chain). Set
  `RAYON_NUM_THREADS=4` to keep the machine usable.

## Deploying a program

LEZ programs are risc0 guest ELFs. A program lives at a *header* account and its bytecode in
*segment* accounts of up to 96 KiB each; you need the private key of all of them, and a funded
payer.

The example programs in `examples/program_deployment` are built by `up.sh --examples` (or
`P=testnet/run testnet/sequencer/build.sh --examples`) into `testnet/run/bin/hello_world.bin`.

**At a header of your choice.** The program id is the header's account id, which depends only on
its public key. To deploy at a fixed id (for example one an external config already names),
import that key and deploy there:

```sh
W=testnet/wallet.sh
$W account import public --private-key <hex>            # the header key; or `account new public`
n=$(( ($(wc -c < program.bin) + 98303) / 98304 ))       # 96 KiB segments; an upper bound (see below)
for i in $(seq 1 $n); do $W account new public --label seg$i; done
$W program-loader deploy --elf program.bin --header Public/<header id> \
   --segments Public/<seg1> ... --payer Public/<operator>
```

Only the program's user ELF is uploaded, not the risc0 kernel that the `.bin` also holds, so the
real count can be one lower than the file size suggests; if it differs, the wallet stops before
sending anything and says how many it needs ("splits into 3 segment(s) but 4 were supplied").

`--private-key` puts the key in your shell history and the process list; type it in a private
shell and clear the history afterwards. Add `--immutable` to declare the program immutable (the
loader records it; it is not enforced by the protocol). `program-loader update` replaces the code
at the same header.

**And call it.** The wallet has no generic "call" command: a program is called with a
transaction built for it (by your app or a small Rust runner using the `wallet` crate). The
runners in `examples/program_deployment/src/bin` (for example `run_hello_world`, which deploys
`hello_world` and calls it) **do not work on v0.4.0-rc3**: they send no fee declaration and the
sequencer refuses the call with "Incorrect fee: transaction fee classification failed".
Upstream reworked them on `dev` after rc3 (commits `41a75c5b` "Fix fee param in deploying example
programs", `45bccb8b`, `c1fd3b01`; not tried here); they don't apply cleanly to rc3, so this
branch doesn't carry them. `smoke.sh --call` tries the runner and
reports that failure; without `--call` the step is a SKIP. A runner of your own must set a
`FeeDeclaration` (payer, gas limit, max fee) on its `Message`, as the wallet's own commands do.

A deploy costs about 24 units per byte of program at base fees of 8 (rc3 meters the loader at
1 gas per byte it reads, on top of the storage gas of each segment).

## Ports, logs, what persists

| | Default | Bound to |
|---|---|---|
| RPC (JSON-RPC over HTTP; also WebSocket) | 3040 | 127.0.0.1 (`--public-bind`: 0.0.0.0; that flag has not been run by us) |
| Metrics (Prometheus) | 3041 | 127.0.0.1 always |
| Filter (`proxy/start.sh`) | 3045 | 127.0.0.1 |
| Faucet (`faucet/start.sh`) | 3046 | 127.0.0.1 |
| Gossip | off | none |

Everything lives under the home (`testnet/run` by default, ignored by git):
`chain/home` (the sequencer's config, channel signing key and RocksDB: the chain itself),
`wallets/operator` (keys), `logs/sequencer.log` (`RUST_LOG=warn` by default; set `RUST_LOG=info`
before `up.sh` for more), `bin/` (the built binaries), `kit.env` (ports, block time, genesis
mode, operator). `down.sh` + `up.sh` keep the chain; `reset.sh` deletes `chain/home` and keeps
the wallets; `reset.sh --all` deletes the home.

A restart resumes at the same height within a few seconds. Keep a stopped chain's home intact:
it can't be rebuilt from anywhere else.

## Build features

`up.sh` builds `sequencer_service --features standalone,prove` and `wallet --features lee/prove`:

- `standalone` replaces the Logos Blockchain client with a mock, so no node is needed.
- `prove` makes risc0 run guests in-process. Without it, risc0 runs every guest through an
  external `r0vm` binary (rzup's, version 3.0.5 for rc3); by the code, the wallet then needs it for
  every simulation too (not run). Checked in CI (job `sequencer-without-prove`, Ubuntu 24.04): a sequencer built with
  `standalone` only **does not start without r0vm**; it panics executing the genesis
  ("Failed to execute genesis transaction: ProgramExecutionFailed(No such file or directory)").
  After `rzup install r0vm 3.0.5` the same binary starts and smoke.sh passes. So a sequencer that
  orders only public transactions still needs either `prove` or r0vm; the kit uses `prove`.

Override with `SEQ_FEATURES` / `WALLET_FEATURES`; `LEZ_BUILD_WRAP="nice -n 19"` prefixes every
cargo call; `CARGO_TARGET_DIR` puts the 20 GB elsewhere.

## Known rc3 quirks

- A transfer `--to` someone else's never-used account would revert; the wallet refuses it (above). Use `--to-pk`.
- A reverted public transaction pays its whole declared gas limit.
- An unpatched rc3 sequencer keeps its RocksDB write-ahead logs until a GB-scale limit and keeps
  every table file open; a long-running chain hits the open-file limit and crash-loops. This
  branch carries the fix ([PATCHES.md](PATCHES.md)).
- Metrics default to `0.0.0.0:9000` and gossip to a UDP socket on `0.0.0.0` in upstream's config;
  the kit sets loopback and off.
- The sequencer's RPC has no auth, CORS or rate limit (upstream issue #992 asks for CORS). Don't
  expose it directly: [PUBLIC.md](PUBLIC.md).

## Letting others use it

[PUBLIC.md](PUBLIC.md): the RPC filter (`proxy/`), TLS through a tunnel or reverse proxy, rate
limits, what never to expose, a faucet (`faucet/`), the watchdog and service files, and the risk
list.

## Running it as a service

`service/lez-testnet-chain.service` (systemd) and `service/local.lez-testnet-chain.plist`
(launchd) run the sequencer from a home `up.sh` created, restart it on failure and raise the
open-file limit. `sequencer/watch.sh` and `sequencer/rotate-logs.sh` are the watchdog and log
rotation.

## Files

| Path | What |
|---|---|
| `up.sh`, `down.sh`, `status.sh`, `reset.sh`, `wallet.sh`, `lib.sh` | the commands above |
| `smoke.sh` | the end-to-end check |
| `sequencer/build.sh` | builds the binaries from this checkout, cached |
| `sequencer/chain.sh` | setup/start/stop/status of the sequencer in a home |
| `sequencer/watch.sh`, `sequencer/rotate-logs.sh` | watchdog, log rotation |
| `proxy/` | the RPC filter, its tests, `start.sh` |
| `faucet/` | the faucet, its tests, `start.sh` |
| `cloudflared/config.example.yml` | a tunnel that routes only `POST /` to the filter |
| `service/` | systemd and launchd templates |
| `PATCHES.md`, `SYNC.md` | the patches on top of upstream; how to move to a new upstream tag |
| `EXAMPLE-RUN.txt` | a full run from a clean clone |

## License

LEZ is licensed "MIT or Apache-2.0" (the workspace `Cargo.toml`; `LICENSE` at the repository
root is the MIT text). Everything in `testnet/` and every patch on this branch is offered under
the same terms.
