# Moving this branch to a new upstream release

`testnet-0.4` is upstream's tag + our patch series + the kit. When upstream tags a new v0.4
release candidate, the branch is rebuilt on it. The `upstream-watch` workflow does the first
checks daily and reports on one issue of the fork ("upstream moved: ..."); a person does the
rest. Nothing is ever force-pushed automatically.

The pin lives in `testnet/UPSTREAM` (tag, commit, patched files).

## Steps

```sh
git remote add upstream https://github.com/logos-blockchain/logos-execution-zone.git   # once
git fetch upstream dev --tags
. testnet/UPSTREAM; NEW=v0.4.0-rcN                                   # the new tag
git log --oneline "$TAG..testnet-0.4" -- . ':!testnet' ':!.github' ':!README.md'   # our patches
```

1. **Drop what upstream merged.** For each patch, check whether upstream has it (the PR link in
   PATCHES.md, or `git log $NEW -- <file>`). Merged patches are dropped in the next step.
2. **Rebase onto the new tag**, on a new local branch, never on `testnet-0.4` directly:
   ```sh
   git checkout -b sync-$NEW testnet-0.4
   git rebase --onto "$NEW" "$TAG"          # drop merged patches here (git rebase -i if you prefer)
   ```
   A conflict in a patch means upstream changed the same code: redo the fix on the new code,
   keep it one commit, and update its "Applies to" in PATCHES.md.
3. **Bump the pin:** `testnet/UPSTREAM` (`TAG`, `COMMIT=$(git rev-parse $NEW^{commit})`,
   `PATCHED_FILES`), and anything versioned in the kit: the risc0 version in README.md and the
   workflows (`risc0-zkvm` in the root `Cargo.toml`; rzup must install the same version), the
   Rust version (`rust-toolchain.toml`), the RPC method list in `proxy/lez-rpc-proxy.py` and
   PUBLIC.md (compare with `lez/sequencer/service/rpc/src/lib.rs`), `--features` names in
   `sequencer/build.sh` (`lez/sequencer/service/Cargo.toml`), the config fields `chain.sh` edits
   (`lez/sequencer/service/networks/local.json`), and the wallet commands smoke.sh uses.
4. **Rebuild and run:** `testnet/reset.sh --all --yes; testnet/up.sh --examples --rebuild &&
   testnet/smoke.sh && testnet/proxy/test-live.sh`. All must pass.
5. **Note it** in PATCHES.md (applies-to commit of every patch, status of dropped ones) and in
   README.md (the tag it names).
6. **Publish:** push `sync-$NEW` and open a PR into `testnet-0.4` on the fork so CI runs; when
   green, a maintainer replaces `testnet-0.4` with it (`git push --force-with-lease origin
   sync-$NEW:testnet-0.4`). A chain running from the old branch keeps working until its
   operator rebuilds; an existing chain home usually can't be opened by a new release (LEZ does
   not promise state compatibility between release candidates), so moving a chain means a new
   genesis (`reset.sh`) unless the release notes say otherwise.

## The patch branches

Each patch also lives on its own branch based on upstream `dev` (`fix/<name>`), for the upstream
PR. When `dev` moves: `git rebase upstream/dev fix/<name>`, rebuild, rerun its check, and
force-push that branch only (it is ours and only feeds the PR).
