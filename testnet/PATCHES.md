# Patches on top of upstream

Branch `testnet-0.4` = upstream tag **`v0.4.0-rc3`** (`bafd07be`) + the patch commits below, one
fix each, + the `testnet/` kit (and its workflows) on top. Each patch is also a branch of its
own, based on upstream `dev`, ready to be sent upstream as a pull request. When upstream merges
one, it is dropped here at the next sync ([SYNC.md](SYNC.md)).

`git log --oneline v0.4.0-rc3..testnet-0.4 -- . ':!testnet' ':!.github' ':!README.md'` lists them
(patch 2 came after the kit commits; the next sync puts every patch first).

| # | Patch | Why | Evidence | Applies to | Upstream | Status |
|---|---|---|---|---|---|---|
| 1 | `fix(sequencer): cap RocksDB WAL size and open files in the sequencer store` (`lez/sequencer/actors/storage/src/actor/db.rs`, +11 lines) | The store opens RocksDB with default options: `max_total_wal_size` 0 (a dynamic limit of several GB), every table file kept open, 1000 info logs. The column families get small writes each block and never fill a memtable, so write-ahead logs pile up until that limit; and under a low descriptor limit (macOS: 256) opens fail, compaction stops and each crash-restart leaves more files behind. | See below | `bafd07be` (v0.4.0-rc3): applies, built (`--features standalone,prove`) and run. Upstream `dev` (`cb075d57`): applies; `sequencer_storage_actor` builds, clippy `-D warnings` and fmt clean (the sequencer was not run on dev) | not sent yet; branch `fix/rocksdb-wal-cap` on this fork, based on `dev` | carried |
| 2 | `fix: example deployer to count elf-only bytes like wallet` (`examples/program_deployment`, +8/-1, and its `Cargo.lock` line) | The example's `deploy_program` counts segments from the whole `.bin` while the wallet uploads only the user ELF, so `run_hello_world` fails ("splits into 3 segment(s) but 4 were supplied") for the `hello_world` guest. Needed for the runner to get past the deploy; on rc3 its call then fails for a separate reason (no fee declaration; README, Deploying a program) | the failure, seen on this kit's chain | backport of upstream `7b916154` | already on upstream `dev` | carried until the next tag includes it; drop at sync |

## 1. RocksDB WAL and open-file cap

Measured (all standalone sequencers, nothing deployed unless said):

- **rc3 (`bafd07be`) unpatched vs patched, same conditions**, measured 2026-11-13: a standalone
  sequencer started by this kit's `chain.sh` with LEZ's local genesis, 2 s blocks, nothing
  deployed, sampled every 30 s for 15 minutes (`max_total_wal_size`, `max_open_files` and
  `keep_log_file_num` read back from RocksDB's `OPTIONS` file in each home: 0 / -1 / 1000
  unpatched, 268435456 / 200 / 10 patched):

  | Height | Unpatched home (WAL files) | Patched home (WAL files) |
  |---:|---:|---:|
  | 155-165 | 305 MB (6) at 165 | 285 MB (6) at 155, its peak |
  | 363-365 | 647 MB (21) at 365 | 77 MB (4) at 363 |
  | about 450-536 | 965 MB (38) at 536, growing about 1.8 MB a block | 250 MB (13) at 454 |

  Unpatched, the write-ahead logs grow linearly with the height (each block writes about
  1.8 MB of WAL); patched, the home cycles between about 75 and 285 MB. The durable data
  (sst + blob) was 1.6-9 MB in both. At 15 s blocks that unpatched rate is about 10 GB of WAL a
  day until RocksDB's own limit. Open files stayed under 45 in both runs (too short to reach the
  descriptor problem; see the rc1 evidence below).
- **rc3 unpatched, another chain:** 6 WAL files kept, a 117 MB home after 41 blocks.
- **rc1 unpatched, a chain with programs deployed, 22 hours of heavy use:** a sawtooth of
  write-ahead logs peaking near 4 GB (93 files, 2.93 GB; 117 files, 3.98 GB; purged to 49 files,
  1.79 GB about every 32 minutes) beside 97-108 MB of durable data. RocksDB's `OPTIONS` file
  showed `max_total_wal_size=0`, `max_open_files=-1`, `keep_log_file_num=1000`.
- **rc1, the same chain before and after the patch** (same home, same height): 211 MB → 14 MB
  at the first start of the patched build.
- **The failure it prevents:** an earlier long-running LEZ chain under launchd (soft limit 256
  open files) hit the descriptor limit; RocksDB opens failed, the sequencer crash-looped 3,895
  times and left 16 GB of junk files.

The change: `set_max_total_wal_size(256 MB)` forces flushes so old logs can be deleted;
`set_max_open_files(200)` keeps RocksDB under small descriptor limits (it closes and reopens
table files through its table cache instead); `set_keep_log_file_num(10)` keeps 10 info logs
instead of 1000. Options only, read at open: an existing home keeps its data, and the patched
binary can start on an unpatched home and the other way round.
