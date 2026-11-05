# A session dir written by ecp 0.17.3 in two worktrees

`sessions/v0173sess/` is a byte-for-byte copy of what the release binary
`ecp 0.17.3+bac7290` (the installed `~/.local/bin/ecp`) left in its cache.
Nothing in `sessions/` is written by code in this repository.
`stored_formats_prev_release.rs` (`session_written_by_0_17_3`) reads it with
the current code.

Date: 2026-11-05. Linux x86_64.

## Generation

1. A git repository with `source/committed/` committed with fixed author,
   committer and dates (`2026-01-01T00:00:00Z`), so the commit id is
   `5fe69c579c8e470a2bdf8921acf40988d7bb2ea0` on any machine. The test asserts
   this id against `session_meta.json` `base_sha`.
2. `git worktree add -b side ../wtB`.
3. `HOME=<tmp>/home ECP_SKIP_BG_REBUILD=1 ECP_SESSION_ID=v0173sess` (no
   `CLAUDE_CODE_SESSION_ID`, no `ECP_HOME`):
   - `ecp find keep_me --repo <A>` on the clean tree (cold index).
   - `app.py` in A replaced by `source/uncommitted-a/app.py`;
     `ecp find keep_me --repo <A>`. stderr: `l1.refreshed written=1`.
   - `lib.py` in B replaced by `source/uncommitted-b/lib.py`;
     `ecp find helper_b --repo <B>`. stderr: `l1.refreshed written=1`.
4. `sessions/` copied from `<tmp>/home/.ecp/repo__a8165edd/`.

The second query replaced the first: the old manifest names only `lib.py`.
That is the defect the new layout (`sessions/<sid>~<worktree key>/`) removes.
`session_meta.json` stores the writer's absolute scratch path and pid.

## What this sample cannot show

Written before the test ran.

- Two worktrees on one commit. Worktrees on different commits (the old
  rebase and wipe path) are not in it.
- Python only, one file per worktree. None of the other 13 mainstream
  languages. The test checks directory naming and isolation, which do not
  depend on the language.
- No fragment of one worktree carrying another worktree's mtime.
- No peer inbox, `msg.log` or agent name in the old dir.
- A `base_sha` that is indexed. A session whose commit is not indexed is not
  in it.
- Linux only; the test sets no Windows path case.
