# Concurrency Audit — Findings

**Date started:** 2026-05-16
**Spec:** [2026-05-16-concurrency-audit-design.md](./2026-05-16-concurrency-audit-design.md)
**Status:** Open

## §3 Inventory pass

### 3.1 Rayon parallel iterators

| file:line | snippet | verdict | reason |
|-----------|---------|---------|--------|
| `crates/graph-nexus-core/src/analyzer/pipeline.rs:262` | `files.into_par_iter().for_each_with(tx, ...` | safe | closure reads `&self` (immutable provider lookup), owns all output via crossbeam sender; no shared mutable state |
| `crates/graph-nexus-cli/src/commands/search.rs:586` | `embeddings.par_iter().enumerate().filter_map(` | safe | closure reads shared slice + `q_norm` (f32 copy), owns `(idx, f32)` output; no writes to shared state |
| `crates/graph-nexus-cli/src/commands/search.rs:785` | `loaded.par_iter().map(|(...)| ...)` | safe | closure reads shared `&[(String, Result<Engine,_>)]`, owns `Vec<Hit>` output; `dispatch_by_mode` takes `&ArchivedZeroCopyGraph` |
| `crates/graph-nexus-analyzer/src/resolution/builder.rs:627` | `// par_iter so the inner closure only needs read` | safe | comment explaining pre-computation strategy; not a callsite itself |
| `crates/graph-nexus-analyzer/src/resolution/builder.rs:685` | `// par_iter worker so each thread owns its own` | safe | comment documenting per-thread `Resolver` construction; not a callsite itself |
| `crates/graph-nexus-analyzer/src/resolution/builder.rs:734` | `local_graphs.par_iter().enumerate().flat_map_iter(` | safe | each worker constructs its own `Resolver` (no sharing); reads `&symbol_table`, `&reason_cache` (immutable after pre-computation); owns `Vec<Edge>` output |

### 3.2 Interior mutability (RefCell / Cell / UnsafeCell)

**Pattern note**: 19 per-language tree-sitter parsers follow the same `thread_local! { static PARSER: RefCell<tree_sitter::Parser> }` pattern. `tree_sitter::Parser` is `!Send`, so `thread_local!` is the only correct sharing pattern under rayon. Each rayon worker thread instantiates its own `Parser` lazily; no cross-thread sharing is possible by construction. The per-row reasons below abbreviate this as "same `thread_local!` pattern".

| file:line | snippet | verdict | reason |
|-----------|---------|---------|--------|
| `crates/graph-nexus-analyzer/src/cairo/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | inside `thread_local!` block; each rayon worker thread gets its own instance; no cross-thread sharing |
| `crates/graph-nexus-analyzer/src/rust/parser.rs:12` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern as cairo |
| `crates/graph-nexus-analyzer/src/c_sharp/parser.rs:50` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/dart/parser.rs:44` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/java/parser.rs:13` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/verilog/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/hcl/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/cpp/parser.rs:32` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/zig/parser.rs:11` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/sql/parser.rs:18` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/dockerfile/parser.rs:9` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/crystal/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/swift/parser.rs:50` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/go/parser.rs:21` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/javascript/parser.rs:16` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/ruby/parser.rs:39` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/vyper/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/move_lang/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/solidity/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/bash/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-cli/src/commands/scan_filters.rs:167` | `"RefCell", "Cell", "HashMap", "BTreeMap"` | single_owner_safe | string literal inside a static deny-list array; no RefCell instance, just the word as data |
| `crates/graph-nexus-analyzer/src/kotlin/parser.rs:25` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/c/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/python/parser.rs:78` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/lua/parser.rs:10` | `static PARSER: std::cell::RefCell<tree_sitter` | thread_local_safe | same `thread_local!` pattern |
| `crates/graph-nexus-analyzer/src/resolution/resolver.rs:3` | `use std::cell::RefCell;` | single_owner_safe | import line only; verdict determined by usage at :52 |
| `crates/graph-nexus-analyzer/src/resolution/resolver.rs:52` | `decisions: Option<RefCell<Vec<ResolverDecision>>>` | single_owner_safe | field is `!Sync`; the parallel path in builder.rs constructs a fresh `Resolver` per worker (no sharing); dump path (serial) is the only path where `decisions` is `Some` |
| `crates/graph-nexus-analyzer/src/resolution/resolver.rs:82` | `self.decisions = Some(RefCell::new(Vec::new()));` | single_owner_safe | only called by `enable_dump()` on the serial dump path; never called on the parallel path |
| `crates/graph-nexus-analyzer/src/resolution/resolver.rs:88` | `self.decisions.take().map(RefCell::into_inner)` | single_owner_safe | drains the buffer on the serial path after all work is done; single-threaded access confirmed |

### 3.3 Unsafe blocks

| file:line | snippet | verdict | reason |
|-----------|---------|---------|--------|
| `crates/graph-nexus-core/src/daemon.rs:22` | `unsafe { cmd.pre_exec(\|\| { nix::unistd::setsid` | documented_safe | `pre_exec` closure runs after fork in child process only (single-threaded by POSIX fork semantics); `setsid()` is async-signal-safe; comment absent but invariant is structural |
| `crates/graph-nexus-core/src/analyzer/pipeline.rs:367` | `unsafe { std::env::set_var("GNX_MAX_FILE_BYTES"` | suspect | test-only; `SAFETY` comment claims no race with other tests, but the sibling test at line 325 calls `pipeline.analyze()` which reads this env var; cargo test runs tests in parallel by default within a crate — true data race on process-global env |
| `crates/graph-nexus-core/src/analyzer/pipeline.rs:399` | `unsafe { std::env::remove_var("GNX_MAX_FILE_BYTES"` | suspect | paired cleanup for :367; same race window — if parallel test reads env between set_var and remove_var it sees the poisoned value |
| `crates/graph-nexus-cli/src/engine.rs:24` | `let mmap = unsafe { Mmap::map(&file)? };` | documented_safe | `Mmap::map` requires `unsafe` by API contract (UB if file is modified while mapped); file is an index artifact written atomically via rename; no writer modifies in-place while mmap lives; invariant held structurally by atomic-write discipline elsewhere |

### 3.4 Shared mutex / atomic state

| file:line | snippet | verdict | reason |
|-----------|---------|---------|--------|
| `crates/graph-nexus-analyzer/src/embeddings.rs:122` | `model: Mutex::new(model),` | bounded_contention | `TextEmbedding` model wrapped in `Mutex`; called sequentially from indexing pipeline (one batch at a time); no concurrent callers observed in codebase — effectively serialized |
| `crates/graph-nexus-cli/src/commands/admin/index.rs:278` | `// misleading "min(pre, post)" upper bound). \`AtomicUsize\`` | not_used_concurrently | comment line only; verdict from :280 |
| `crates/graph-nexus-cli/src/commands/admin/index.rs:280` | `let cache_hits_counter = std::sync::atomic::AtomicUsize::new(0);` | bounded_contention | `AtomicUsize` used as a counter across rayon workers via `Ordering::Relaxed` fetch_add; relaxed ordering is correct for a pure counter (no ordering dependency on other memory); single writer window, read once after `rayon::join` completes |

### 3.5 File locks

| file:line | snippet | verdict | reason |
|-----------|---------|---------|--------|
| `crates/graph-nexus-core/src/registry/mod.rs:16` | `pub use lock::FileLock;` | raii_safe | re-export; verdict from lock.rs implementation |
| `crates/graph-nexus-core/src/registry/mod.rs:27` | `// registry.json under flock protection.` | raii_safe | doc comment only |
| `crates/graph-nexus-core/src/registry/mod.rs:58` | `/// Insert or update a repo entry. Holds exclusive` | raii_safe | doc comment only |
| `crates/graph-nexus-core/src/registry/mod.rs:62` | `let _lock = FileLock::acquire_exclusive(&lock_path)` | raii_safe | `_lock` keeps file open; `FileLock` wraps `File` with no explicit `unlock` — released on drop; panic during write would unwind and drop the lock cleanly |
| `crates/graph-nexus-core/src/registry/lock.rs:4` | `use fs2::FileExt;` | raii_safe | import; `fs2` advisory locks are released when the `File` fd is closed, which happens on `FileLock` drop |
| `crates/graph-nexus-core/src/registry/lock.rs:10` | `pub struct FileLock { _file: File }` | raii_safe | RAII struct; lock released when `_file` drops; no `ManuallyDrop`, no `mem::forget` risk observed at callsites |
| `crates/graph-nexus-core/src/registry/lock.rs:14` | `impl FileLock {` | raii_safe | impl block header for the RAII struct at :10; no separate concern beyond the struct's drop semantics |
| `crates/graph-nexus-cli/src/background.rs:3` | `// non-blocking \`flock\` so concurrent triggers no-op` | raii_safe | doc comment only; shell-level `flock -n 9` in subprocess script |
| `crates/graph-nexus-cli/src/background.rs:21` | `/// Non-blocking \`flock\` target. If another process` | raii_safe | doc comment only |
| `crates/graph-nexus-cli/src/background.rs:48` | `flock -n 9 \|\| exit 0` | raii_safe | shell script embedded in string literal; non-blocking flock exits 0 on contention — no deadlock possible; shell process lifetime bounds the lock |
| `crates/graph-nexus-cli/src/background.rs:76` | `flock -n 9 \|\| exit 0` | raii_safe | same as :48 |
| `crates/graph-nexus-cli/src/commands/hook/post_tool_use.rs:95` | `/// Detached background \`gnx admin index\` under flock` | raii_safe | doc comment only |
| `crates/graph-nexus-cli/src/commands/hook/post_tool_use.rs:129` | `/// Detached background \`gnx admin prune\` under flock` | raii_safe | doc comment only |
| `crates/graph-nexus-cli/src/commands/admin/prune.rs:80` | `let _lock = graph_nexus_core::registry::FileLock::` | raii_safe | same RAII pattern; lock held for read-modify-write of registry.json then released on scope exit |
| `crates/graph-nexus-cli/src/commands/admin/prune.rs:81` | `.map_err(\|e\| GnxError::InvalidArgument(format!` | raii_safe | error-path continuation of :80 |
| `crates/graph-nexus-cli/src/commands/admin/group.rs:2` | `use graph_nexus_core::registry::{..., FileLock,` | raii_safe | import line |
| `crates/graph-nexus-cli/src/commands/admin/group.rs:26` | `let _lock = FileLock::acquire_exclusive(&lock_path)` | raii_safe | same RAII pattern; `mutate_registry` helper scopes lock to its frame |
| `crates/graph-nexus-cli/src/commands/admin/group.rs:27` | `.map_err(\|e\| GnxError::InvalidArgument(format!` | raii_safe | error-path continuation of :26 |
| `crates/graph-nexus-cli/src/commands/admin/index.rs:381` | `let _lock = graph_nexus_core::registry::FileLock::` | raii_safe | lock acquired before rkyv serialize + atomic rename; released after `atomic_write_bytes` returns; panic during serialize unwinds and drops lock |
| `crates/graph-nexus-cli/src/commands/admin/drop.rs:6` | `//   * rewrite \`registry.json\` without that entry` | raii_safe | doc comment only |
| `crates/graph-nexus-cli/src/commands/admin/drop.rs:57` | `// Drop registry handle before acquiring exclusive` | raii_safe | comment documenting intentional drop ordering to avoid double-lock; correct pattern |
| `crates/graph-nexus-cli/src/commands/admin/drop.rs:74` | `/// Re-read registry.json under exclusive flock,` | raii_safe | doc comment only |
| `crates/graph-nexus-cli/src/commands/admin/drop.rs:81` | `let _lock = graph_nexus_core::registry::FileLock::` | raii_safe | same RAII pattern inside `rewrite_without`; explicit `drop(registry)` before lock acquisition avoids any aliasing |
| `crates/graph-nexus-cli/src/commands/admin/drop.rs:82` | `.map_err(\|e\| GnxError::InvalidArgument(format!` | raii_safe | error-path continuation of :81 |

### 3.6 Process / thread spawn

| file:line | snippet | verdict | reason |
|-----------|---------|---------|--------|
| `crates/graph-nexus-core/src/daemon.rs:15` | `let mut cmd = Command::new(args[0]);` | fire_forget_ok | `spawn_detached` calls `cmd.spawn()` and immediately drops the `Child` handle; intent is fire-and-forget daemon; no stdout/stderr captured, no join needed |
| `crates/graph-nexus-cli/src/admin/diagnostics.rs:39` | `let output = Command::new(exe).args(["mcp", "tools"])` | fire_forget_ok | uses `.output()` which waits synchronously for exit; not truly "spawn" — blocks until done; no lifetime risk |
| `crates/graph-nexus-cli/src/admin/diagnostics.rs:191` | `match Command::new(command).args(args).output()` | fire_forget_ok | same `.output()` synchronous pattern; used for version probing |
| `crates/graph-nexus-cli/src/admin/host_integration/mcp/claude_code.rs:44` | `let output = Command::new("claude").args(args).output()` | fire_forget_ok | `.output()` waits synchronously; install operation, result checked immediately |
| `crates/graph-nexus-cli/src/admin/host_integration/mcp/claude_code.rs:66` | `Command::new("claude").args(args).output()` | fire_forget_ok | same `.output()` pattern in `claude_mcp` helper |
| `crates/graph-nexus-cli/src/background.rs:94` | `Command::new("sh").arg("-c").arg(&shell)...spawn()` | fire_forget_ok | shell subprocess runs `flock -n 9` guard before doing work; `spawn()` returns immediately; child is detached (no join); fire-and-forget by design — marker files signal completion |
| `crates/graph-nexus-cli/src/git/safe_exec.rs:16` | `let mut cmd = Command::new("git");` | fire_forget_ok | `git()` factory returns a `Command` builder; actual execution is at callsite (`.output()` or `.status()`); factory itself does not spawn |
| `crates/graph-nexus-cli/src/commands/diff/baseline.rs:83` | `let out = Command::new("gh").args([...]).output()` | fire_forget_ok | `.output()` synchronous; result checked; no orphaned child |
| `crates/graph-nexus-cli/src/commands/diff/bindings.rs:60` | `let out = Command::new(&self_exe).args([...]).output()` | fire_forget_ok | synchronous `.output()`; re-invokes self as `gnx admin index --dump-resolver`; result checked |
| `crates/graph-nexus-cli/src/commands/hook_watcher.rs:53` | `let mut cmd = std::process::Command::new(&gnx_bin);` | fire_forget_ok | `.output()` called at :62 (`let _ = cmd.output()`); result intentionally discarded (best-effort rename/prune); synchronous wait, no orphaned child |

## §4 Hot-path equivalence test results

### 4.1 pass2_parallel_serial_identical_per_reltype

**Existing fixture covered:** Calls, Extends, Accesses, References (×2)
**Extended fixture covers (additionally):** HandlesRoute (bar.rs GET /users → other_fn via `detect_from_call` + `lookup_in_file`)
**Asserts emit-zero invariant for:** Implements, Imports, Defines, Fetches (zero-edge sanity; Sub-projects 1, 5 will lift these)
**Stratification:** per `RelType` BTreeMap, asserted independently
**Equality includes:** `(source, target, RelType, resolved_reason_string)`
**Result:** PASS default threads / PASS --test-threads=1 / PASS --test-threads=16

### 4.2 GraphBuilder ingest-order independence

| Sub-test | default | --test-threads=1 | --test-threads=N |
|----------|---------|------------------|-------------------|
| `graph_builder_order_independence_under_default_threads` | FAIL | FAIL | FAIL |
| `graph_builder_repeated_build_is_stable` | PASS | PASS | PASS |

**Canonical projection:** sorted (Nodes by `(uid, name, kind, span, file_idx)`, Edges by `(rel_type, source, target, reason)`, Files by path) → BLAKE3 hash. Rkyv padding excluded.

**Audit cross-check:** parallel path's `flat_map_iter` output order is non-deterministic by rayon design; `build()` sort-and-archive normalises it. The canonical projection is what consumers actually see.

**Result summary:** `graph_builder_repeated_build_is_stable` passes — same-order builds are byte-stable across 5 repeated runs. `graph_builder_order_independence_under_default_threads` fails with identical mismatched hashes on all thread counts, confirming this is a structural determinism bug, NOT a rayon scheduling flake. See `inv-003` in §7.

### 4.4 StringPool concurrent intern

| Sub-test | default | --test-threads=1 | --test-threads=N |
|----------|---------|------------------|-------------------|
| `string_pool_serial_dedupe_holds_under_pressure` | PASS | PASS | PASS |
| `string_pool_mutex_wrapped_concurrent_dedupe` | PASS | PASS | PASS |

**Invariant pinned:** Pool MUST be `Mutex`/`RwLock` wrapped when shared. Direct cross-thread `&mut StringPool` is forbidden by the borrow checker (`add()` signature is `&mut self`).

**Audit cross-check of pass2 production path:** `crates/graph-nexus-analyzer/src/resolution/builder.rs:620-740` parallel path pre-interns all reasons serially BEFORE entering `par_iter`, then shares only `&StrRef`. No `StringPool` mutation in worker. ✓ safe by construction.

## §5 TSan results
(populated by Phase 7)

## §6 Performance findings (surfaced)
(populated by Phase 8)

## §7 Bugs found

| ID | Source | Hypothesis | Status |
|----|--------|------------|--------|
| inv-001 | `crates/graph-nexus-core/src/analyzer/pipeline.rs:367` (axis §3.3) | `unsafe { std::env::set_var("GNX_MAX_FILE_BYTES", "10") }` in test `oversize_file_is_skipped` races with the sibling test (line 325) which calls `pipeline.analyze()` → `resolve_max_file_bytes()` → `std::env::var(...)` under parallel test execution (cargo test default). Both tests are in the same crate test binary; no `#[serial]` guard or `--test-threads=1` annotation. Outcome: sibling test may see the poisoned 10-byte cap and silently skip files, producing a false-failing assertion. Fix: wrap the set/remove in a mutex-guarded serial block (e.g. `serial_test` crate) or isolate in a separate integration test binary. | needs_verification |
| inv-002 | `crates/graph-nexus-core/src/analyzer/pipeline.rs:399` (axis §3.3) | Paired `unsafe { std::env::remove_var(...) }` cleanup for inv-001's `set_var`. Same race surface — if a parallel sibling test reads the env var between :367 set and :399 remove, it sees the poisoned value. Same hypothesis and same fix as inv-001 (single serial guard covers both sites; do not fix one without the other). | needs_verification |
| inv-003 | `crates/graph-nexus-analyzer/src/resolution/builder.rs:185-934` (axis §4.2) | `build()` assigns node indices (`source`/`target` in `Edge`) sequentially in file-ingest order (`self.local_graphs.iter().enumerate()`). UIDs are content-derived (`format!("{:?}:{}:{}", kind, path, name)`) and are ingest-order-independent, but edge endpoints use absolute integer node indices which are insertion-order-dependent. Reversing input files shifts all node indices: what was node 0 becomes node 14. The `build()` sort (`edges.sort_by_key(\|e\| e.source)`) is a CSR-construction sort over integer indices, not a semantic normalisation. Result: canonical hash of `(rel_type, source_idx, target_idx, reason)` differs when input order differs. **Fix hypothesis:** after all nodes are registered, sort `nodes` by `uid` string, remap all edge endpoints through the new position map, then proceed. Alternatively: sort `local_graphs` by `file_path` before processing in Pass 1 (simpler but requires caller agreement). The `uid` string is already fully content-derived and path-stable. | open |

## §8 Closure checklist
- [ ] All §3 axes populated
- [ ] All 5 §4 tests PASS under `--test-threads=1` and `--test-threads=N`
- [ ] Zero unfiltered TSan reports
- [ ] All §7 bugs have merged fixes
- [ ] All §6 perf items have follow-up issues filed (or marked documented-tradeoff)
