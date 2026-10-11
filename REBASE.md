# REBASE — moving this branch onto a new upstream base

This branch is the fork's headline line. It rebases with a fixed procedure; follow it in
order and every gate is one command (`tools/verify.sh`). The stable toolchain is mandatory
everywhere: the default nightly is too old for `epaint 0.36.2` — every cargo call below is
`cargo +stable`, one at a time.

## 1. Back up before touching any ref

```sh
git tag pre-rebase-<base>-clean-room <tip>
git push fork --tags
git bundle create ~/.local/share/lightcraft-backups/lightcraft-clean-room-<date>.bundle --all
git bundle verify <that bundle>          # must say "records a complete history"
echo <that bundle> > ~/.local/share/lightcraft-backups/LATEST
```

Nothing below runs until the tag is on `fork` and the bundle verifies — every later step
(including branch deletes) leans on them as the reachability proof. Delete superseded
bundles in that directory after the new one verifies.

## 2. Ground truth first: the scratch merge

Do **not** rebase blind. Build the merged tree first, resolve every conflict on the merits
(upstream's change and this branch's both belong), and only then replay the commits.

```sh
git worktree add -b scratch/rb-merge <path> <tip>     # a scratch worktree, not a shared one
cd <path>
git merge origin/main                    # resolve everything; on the merits, never "ours/theirs"
cargo +stable check --workspace          # the auto-merge can compile-break: fix it here
git commit --amend --no-edit             # semantic fixes belong in the merge commit itself
```

Known trap classes, from previous rebases:

- **Version collisions.** If both sides bumped the same constant for different reasons
  (`RENDER_CACHE_VERSION`: upstream 60→61 for the JPEG XL decode fix, the colour NR 60→61),
  the branch must ship a value that distinguishes its caches from every state the user can
  run — take `max(both) + 1` and put it in whichever commit owns the bump. Check every
  version-ish hunk after the replay:
  `git log -L '/pub const RENDER_CACHE_VERSION/,+1:crates/engine/src/media.rs' <base>..<tip>`.
- **Format bumps** (journal/settings formats): same rule as above.
- **Window-geometry test harnesses** (1400×800, `folders_app_sized`, `ViewMode::Detail` —
  see `FORK-NOTES.md`, "Deliberate upstream test divergences"): the docking and panel
  tests pin fixed geometry; an auto-merged upstream harness change can break them without
  a compile error. Fix the harness only when the layout change is intended.
- **Locales**: upstream's keys stay in upstream's order; this branch's keys ride at the end
  of the file; the shared keys' values stay JSON-equal to upstream's.

## 3. Replay the line

The scratch merge must live on its **own branch**, or the rebase range swallows upstream's
history. Then replay with the branch tip:

```sh
git branch -f scratch/rb-merge <the merge commit>     # keep the ground truth reachable
git worktree add -b scratch/rb-replay <path> <tip>
git -C <path> rebase --onto origin/main <base-of-the-line> scratch/rb-replay
```

Conflicts during the replay: rerere answers the ones the scratch merge already resolved;
the rest are resolved on the merits.

### The proof

The replayed tip must equal the scratch merge apart from named exceptions:

```sh
git diff scratch/rb-merge <replayed-tip>
```

Every difference is named with a reason in the report — on the 2026-10-11 rebase that was
the locale files (key order only, content-equal: deliberate) and the tracker-doc refresh.
Anything else is a bug in the replay — fix it before continuing.

## 4. Verify

```sh
tools/verify.sh
```

fmt, clippy `-D warnings`, the workspace tests, `cargo xtask parity`, `cargo xtask layers`
and the negative-conversion fixture floors (this branch's own values,
`tools/fixtures.txt`). A failed upstream async-race test (`tests_model_setup`,
`tests_preview_limit`) is rerun once before the gate fails — they lose timing races under
a fully parallel run roughly one run in four and are untouched by this branch. Fix test
failures only when the product behaviour is intended.

Smoke the renders by hand (one from a real camera raw, one film-negative conversion) — the
files are the user's own.

## 5. Push and tag

```sh
git push fork +scratch/rb-replay:clean-room
git tag clean-room-<new-base> && git push fork clean-room-<new-base>
```

`clean-room` is the fork's default branch; keep it so
(`gh repo edit dmitrii-galantsev/lightcraft --default-branch clean-room`). The per-concern
branches (`raw-noise-and-metadata`, `noise-reduction`, `lens-corrections`,
`negative-conversion`) are pointers into this branch at the concern's last commit — find
the commit by subject on the replayed branch and push it force:

```sh
git push fork +<commit>:noise-reduction
```

## 6. Prune

Delete a branch only after its tip is reachable from a preserved ref:

```sh
git merge-base --is-ancestor <branch-tip> pre-rebase-<base>-clean-room   # or the bundle
```

If a branch holds anything not in a preserved ref, stop and report instead of deleting.
Leave upstream's mirrored branches (`claude/*`, `fix/*`, `docs/*`, `feat/*`, `pr/*`,
`release`, `main`) alone — they are not ours.

## What stays manual, honestly

- The scratch merge's on-the-merits resolutions.
- The smoke renders of the user's own files.
- The push/tag steps and the default-branch setting.
