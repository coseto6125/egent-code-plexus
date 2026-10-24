# `commits/` layout written by ecp 0.16.4

`commits/` holds three dirs of one SHA, written by ecp 0.16.4
(`ecp 0.16.4+a9b0c9c`, the binary at `~/.local/bin/ecp` on 2026-10-25). The
reader under test is the SHA stripe lock change: `commits/.locks/<xx>.lock`,
generation names, and gc retiring only under that lock.
`stored_commits_prev_release.rs` reads it.

| Dir | Shape | Writer |
|---|---|---|
| `branch_main__2208…ef99` | bare published dir, generation 0 | `ecp admin index` |
| `branch_main__2208…ef99.gen.1792872205014.1314204.0` | published generation | `ecp admin index --force` |
| `branch_main__2208…ef99.building` | legacy staging dir with a complete `graph.bin`, no `meta.json` | `ecp admin index --force`, killed with SIGKILL |

## Pass criterion (written before the test)

- **Current dir.** Today's reader picks the `.gen.` dir, the dir 0.16.4 picks.
  Evidence for 0.16.4's pick: with all three dirs present and all mtimes set
  to 2026-10-01, 0.16.4 ran `ecp admin index` (`l2.exists`) and
  `ecp find helper`. The find wrote `graph.bin.worktree` into the `.gen.` dir
  only. The bug means the bare dir or the legacy dir is current; the fix
  means the `.gen.` dir is.
- **No rebuild at HEAD.** With today's fingerprint equal to the writer's
  (`v0.16.4+schema20`), `ecp admin index` prints `l2.exists`, not `l2.built`,
  and `ecp find compute` answers `found: true` from `pkg/core.py`. `commits/`
  keeps the same three dirs. With a different fingerprint, the same command
  must build one new generation, and that generation must be current.
- **Legacy dir untouched.** Every file under `<base>.building/` keeps its
  bytes through the index, the query and gc. No file is added or removed.
- **gc retires the older generation only.** `sweep_stale_generations`
  retires the bare dir and keeps the `.gen.` dir and the legacy dir. While
  the legacy dir was written in the last 7 days, it retires nothing, because
  an older ecp may still be building that SHA. The bug means gc retires the
  `.gen.` dir, the legacy dir, or nothing after the legacy dir is idle.

## What was arranged by hand

- **The bare dir beside the `.gen.` dir.** 0.16.4's `--force` publishes a
  `.gen.` dir and then retires the bare dir of the same SHA, so 0.16.4 never
  leaves both on its own. The bare dir was copied aside after the first
  index and copied back after the `--force` run. Both dirs are byte for byte
  what 0.16.4 wrote. The coexistence happens in the field when two 0.16.4
  builds race, or when a build dies between its rename and its sweep.
- **The legacy dir.** It is a real 0.16.4 staging dir: a second
  `ecp admin index --force` was killed with SIGKILL as soon as
  `<base>.building/graph.bin` appeared.
- **Files left out.** The `tantivy/` index of the two published dirs (16
  files each; no assertion reads it, and `find` falls back to a substring
  scan without it), the repo-level `meta.json`, `parse_cache/`,
  `registry.json` and `.build-slots/`. The repo-level files hold absolute
  scratch paths and are not part of the `commits/` layout.
- **`built_from_worktree`.** Each `meta.json` records the scratch repo path
  of the writer, which no longer exists. A slot whose recorded git root is
  gone is rejected by design (`built_below_worktree_root`). The test sets
  that one field to its own repo path through `serde_json::Value`, and keeps
  every other field. The legacy dir has no `meta.json` and is not rewritten.

## What the sample cannot show

- One SHA only. No cross-SHA stripe sharing (two SHAs with the same first
  byte), which the unit tests cover.
- No live 0.16.4 process: the legacy dir's `.build.lock` is free. A legacy
  builder that still holds its lock is covered by `tests/gc.rs`.
- No per-attempt staging dir (`<base>.building.<pid>.<n>`): 0.16.4 never
  writes one.
- No `commits/.locks/`: 0.16.4 writes no lock file directly in `commits/`.
- Mtimes are not stored in git. The test sets them: published dirs one hour
  old (past gc's 10 s in-flight guard), the legacy dir either one hour or
  eight days old.
- Not portable: rkyv bytes are little endian and 64-bit.

## Source repo

The five files of `../stored-by-0.16.4/source/committed/`, committed with:

```
git init -q -b main; git add -A
GIT_AUTHOR_DATE=2026-10-25T00:00:00Z GIT_COMMITTER_DATE=2026-10-25T00:00:00Z \
  git -c user.email=t@t -c user.name=t -c commit.gpgsign=false commit -qm init
# sha 220864493c29195a6e8b6c5cca462e7566a6ef99
```

## Commands (scratch repo `$R`)

```
export ECP_HOME=$R/home2
ecp admin index --repo .            # bare dir
cp -a $ECP_HOME/<key>/commits/<base> $R/aside/
ecp admin index --repo . --force    # .gen.1792872205014.1314204.0; the bare dir is retired
ecp admin index --repo . --force &  # killed with SIGKILL once <base>.building/graph.bin exists
cp -a $R/aside/<base> $ECP_HOME/<key>/commits/
```

Then the files above were copied out with `cp`.
