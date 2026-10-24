# Stored formats written by ecp 0.16.4

Every file under `ecp-home/` is a byte-for-byte copy of what the release binary
`ecp 0.16.4+a9b0c9c` (the installed `~/.local/bin/ecp`) wrote to its cache.
Nothing here is written by code in this repository. The tests in
`crates/ecp-cli/tests/stored_formats_prev_release.rs` read it with the current
code.

Date: 2026-10-25 (files stamped 2026-10-24T18:21Z). Linux x86_64, little endian.

## What this sample cannot show

Written before the tests, as the rule requires. A migration that reads one of
these things sees nothing here, and a green test says nothing about it.

- Languages: only Python, TypeScript and Rust. None of the other 11 of the
  14 mainstream languages. Rust has one file and no `Cargo.toml`, so the
  `graph.bin` Rust crate table (B's first round) is empty here. Lexical
  parents (A) cannot show: no nested function, no class, no method.
- No heritage, enum variant, field read, export alias or `binding_kind` import
  in any fragment. The V5 fields that carry them are empty, so a change to
  their layout reads as unchanged. Fragment return types exist only where the
  Rust file declares one (`-> u32`).
- One fragment format (V5) and one builder. No V2-V4 bin, no entry with
  `parse_failed = true`, no `tantivy_delta_segment`, no entry whose fragment
  file is missing.
- The dirty manifest has no deleted or renamed file and no event class: C3's
  deletion records have nothing to be read against.
- One session, one commit, source type Branch. No tag or PR source, no
  warm-attach sibling, no second session, no `.gen` generation, no stray tmp
  file, no lock contention.
- Same-name symbols: four `helper` functions, one per file, each called only
  from its own file. A bare call that is ambiguous across files (the global
  candidate filter) does not occur. `go` reaches `compute` through an import.
- `graph.bin` is 14 nodes. No route, no entry point, one process, no blind
  spot, no framework reference, no embedding, no alias table content.
- The parse cache holds the five committed files. The uncommitted edits are
  not in it (the overlay fragments serve them). One entry
  (`pkg/__init__.py`) is a near-empty file; no two files share content.
- The view cache key hashes the running executable's path, length and mtime
  and the graph file's path and mtime. Those cannot be reproduced, so a cache
  HIT can never be tested from this fixture. Only the body is read, through
  the stored key.
- `meta.json` and `session_meta.json` hold absolute scratch paths
  (`built_from_worktree`, `source_worktree`) of a directory that no longer
  exists. The tests do not read them.
- The dirty manifest stores the writer's file mtime. The tests rebuild the
  working tree and stamp the same mtime with `set_modified`; they do not read
  the original files.
- Not portable: rkyv bytes are little endian and 64-bit. No Windows path form.

## Contents

```
ecp-home/repo/                              = ~/.ecp/<repo key>/ (renamed to repo)
  commits/branch_main__e7ca0e6a...fed6dc/
    graph.bin                      3428 B   GRAPH_FORMAT_VERSION 15
    graph.bin.compatible_version      3 B   "15"
    graph.bin.builder_fingerprint    17 B   "v0.16.4+schema20"
    graph.bin.head_sha               41 B
    meta.json                       639 B   CommitBuildMeta
  parse_cache/568515f1/*.rkyv       5 files, 176-480 B each (archived LocalGraph)
  sessions/fxsession0001/
    dirty_files.json               1419 B   dirty manifest, fragments format 5
    session_meta.json               468 B
    view_cache.bin                  504 B   VIEW_CACHE_FORMAT 3 body
    graph_overlay/339d025bf804a507.bin  292 B   pkg/core.py fragment (V5)
    graph_overlay/920cf5403de07ab2.bin  192 B   lib.rs fragment (V5)
source/committed/       the five committed files (inputs of graph and parse cache)
source/uncommitted/     lib.rs and pkg/core.py as edited (inputs of the fragments)
```

Left out on purpose: the tantivy index, lock files, `registry.json`,
`.build-slots`, telemetry, `.last-used`. None is one of the five formats.

## Source repo (git, branch main, one commit `init`, sha e7ca0e6a663e619e7b245890db6cb47653fed6dc)

```
app.py          from pkg.core import compute / def go(): return compute() / def helper(): return 1
pkg/__init__.py a single newline
pkg/core.py     def compute(): return helper() + 1 / def helper(): return 2
web/util.ts     export function helper / export function render(): return helper()
lib.rs          pub fn helper() -> u32 / pub fn run() -> u32 { helper() }
```

Exact text is in `source/committed/`. The uncommitted edit appended
`def fresh_only(): return compute()` to `pkg/core.py` (lines 9-10) and a
`// edit` line to `lib.rs`.

## Commands (scratch repo `$R`; `$S` = session scratchpad)

```
R=$(mktemp -d $S/dev/fx.XXXXXX); cd $R; git init -q -b main     # write the files above
git add .; git -c user.email=t@t -c user.name=t commit -qm init
export ECP_HOME=$R/.ecpfx            # relocates the whole cache
ecp admin index --repo .             # graph.bin, parse_cache, meta.json
unset GEMINI_CLI_SESSION_ID CODEX_SESSION_ID CODEX_THREAD_ID CLAUDE_CODE_SESSION_ID
export ECP_SESSION_ID=fxsession0001
# append the uncommitted edits to pkg/core.py and lib.rs
ecp find fresh_only                  # wrote sessions/fxsession0001/{dirty_files.json,graph_overlay,session_meta.json,view_cache.bin}
ecp find helper
```

Then the files above were copied out with `cp`. `graph.bin.head_sha` was
copied from the original cache, not from the working copy used for
exploration (a later `ecp cypher --graph` run from a non-git directory
rewrote the sidecar in that copy).

## Expected contents (what the tests assert)

- `graph.bin`: 14 nodes: 8 Function (`compute`, `go`, `render`, `run`, four
  `helper`), 5 File, 1 Process. Calls: compute->helper (pkg/core.py), go->compute
  (app.py to pkg/core.py), render->helper (web/util.ts), run->helper (lib.rs).
- Dirty manifest: `lib.rs`, `pkg/core.py`; format 5; fingerprint
  `v0.16.4+schema20`.
- Overlay view: 5 virtual nodes, 3 `l1-overlay` Calls edges
  (run->helper, compute->helper, fresh_only->compute).
