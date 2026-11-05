# L1 fragments written by ecp 0.17.3

`l1/` is a byte-for-byte copy of what the release binary `ecp 0.17.3+bac7290`
(the installed `~/.local/bin/ecp`) wrote to its session cache. Nothing here is
written by code in this repository. `crates/ecp-cli/tests/overlay_type_annotation_parity.rs`
reads it with the current code.

Date: 2026-11-05. Linux x86_64, little endian.

## Generation

1. A new git repository with `source/committed/` committed (base commit
   `ebfd6cf48534ebfec89b70adafb305c4fb12078d`).
2. `HOME=<tmp>/home ECP_HOME=<tmp>/home/.ecp ECP_SESSION_ID=v7fixture
   ECP_SKIP_BG_REBUILD=1 ecp find take --format json --repo <tmp>/repo`
   (cold index of the clean tree; `CLAUDE_CODE_SESSION_ID` unset).
3. The two files replaced by `source/uncommitted/`, not committed.
4. The same environment, `ecp find many --format json --repo <tmp>/repo`.
   stderr: `l1.refreshed written=2 failed=0`.
5. `sessions/v7fixture/l1/dirty_events.json` and `l1/fragments/*.bin` copied
   here. Both entries have `"format": 7` and the builder fingerprint
   `v0.17.3+schema52`.

## What this sample cannot show

Written before the tests ran, as the project rule requires.

- Two languages only: Swift and Rust. One file each, no `Cargo.toml`, no
  import. None of the other 12 mainstream languages.
- One fragment format (V7) and one builder. No entry with
  `parse_failed = true`, no tombstone, no entry whose bin is missing.
- The freshness gate's decision to re-parse a V7 entry lives in
  `auto_ensure.rs`; this fixture does not run it. The test re-parses the
  same source with today's writer, as the gate would, and reads that back.
- The type names bind inside one file. No cross-file, imported or qualified
  type. No `Self`. No C# property.
- The manifest stores the writer's absolute scratch path in
  `session_meta.json` (not copied) and the writer's mtime. The test stamps a
  copy of the source with its own mtime and rewrites the entries' `mtime_ns`.
- Not portable: rkyv bytes are little endian and 64-bit.

## Pass/fail criterion

- The V7 entries are refused: the reader serves no input for either file,
  never a V7 bin decoded as the current format.
- After the re-parse, the view's `Accesses/type_annotation` edges out of the
  dirty files equal the full reindex of `source/uncommitted/`:
  `Owner.f -> St`, `take -> St`, `many -> Other` (Swift) and
  `make -> St`, `take -> Other` (Rust). The base edges `f -> Other` and
  `make -> Other` are gone. One missing or one extra edge fails.
