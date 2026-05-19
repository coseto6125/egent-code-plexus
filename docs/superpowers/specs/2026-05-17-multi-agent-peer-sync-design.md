# Multi-Agent Peer Sync — Design

**Status:** Draft (brainstorming output, awaiting user review)
**Date:** 2026-05-17
**Branch:** `feat/peer-sync`
**Scope:** One spec, one PR (~1487 LOC implementation + ~800 LOC tests = ~2287 LOC total)

**Terminology:** "`common_dir`" refers to the git common directory shared by all worktrees of a repository — the path returned by `git rev-parse --git-common-dir` (normally `<repo>/.git`). All worktrees of one repo share the same `common_dir`; this is the basis for auto-peering in §2.

## 1. Motivation

Two or more Claude sessions working on the same repository (typically across git worktrees) cannot currently see each other's in-progress changes until those changes are pushed and pulled through git. This forces coordination through external channels (Slack, PR comments) or hides real-time conflicts until merge time.

This feature lets sessions:

- **Detect symbol-level conflicts in real time** — when peer A modifies a function I am also editing, my next tool call surfaces the conflict before I commit.
- **Exchange short messages** (Ƀ beta) — `cgn peers say "..."` to broadcast intent, request coordination, or hand off work.

The underlying constraint that shapes the entire design: **LLMs have no event loop**. They cannot subscribe, poll, or be interrupted mid-generation. The only reliable injection point is *just before* the next tool call. All transport latency below that boundary is wasted — sub-100ms event delivery is meaningless if the LLM only checks at turn boundaries.

## 2. Design Principles

1. **Filesystem as transport.** Atomic JSON writes + `inotify` (Linux) / `fsevents` (macOS) / `ReadDirectoryChangesW` (Windows) abstracted by the `notify` crate. No daemon, no socket, no shared memory.
2. **Hook injection is the only LLM-facing channel.** All peer state surfaces through Claude Code's `pre_tool_use` / `session_start` / `user_prompt_submit` hooks via `hookSpecificOutput`. Anything that does not flow through a hook is effectively invisible to the LLM.
3. **Symbol-level precision, not file-level.** Concern matching uses the existing graph: HARD if symbols intersect, SOFT if 1-hop graph neighbors intersect, IGNORE otherwise. File-name matching produces too many false positives in monorepos.
4. **Fail-open everywhere.** Any failure in the peer subsystem logs and continues. Losing a peer notification ≪ blocking the user's tool call.
5. **Ephemeral transport, persistent dialog.** `inbox.jsonl` is drain-and-truncate (pure transport). `msg.log` is append-only (audit trail). `dirty.json` and `watcher.log` follow existing patterns.
6. **No group ceremony in v1.** All sessions sharing a git `common_dir` auto-peer. Explicit groups, cross-repo peering, and access control are deferred.

## 3. Storage Layout

Extends the layout established by `#55` (index layout redesign). Per-session directory:

```
~/.cgn/code-graph-nexus/<repo>/sessions/<session_id>/
  ├─ meta.json              # SessionMeta (existing) + watcher_pid field
  ├─ dirty.json             # DirtyFiles (existing) + dirty_symbols field
  ├─ inbox.jsonl            # NEW — transport, drain-and-truncate
  ├─ msg.log                # NEW — append-only, sent + received messages
  ├─ msg.log.{1..7}         # NEW — rotated
  ├─ watcher.log            # NEW — background watcher diagnostic log
  ├─ watcher.log.{1..3}     # NEW — rotated
  └─ watcher.lock           # NEW — flock for single-instance watcher
```

All new files inherit the existing `0700` directory mode from `~/.cgn/`.

## 4. Rust Types

### 4.1 Extended (existing types)

```rust
// crates/cgn-core/src/session/overlay.rs
pub struct DirtyEntry {
    pub mtime_ns: u64,
    pub content_hash: String,
    pub fragment_id: String,
    pub tantivy_delta_segment: Option<String>,
    pub parse_failed: bool,
    // NEW:
    pub dirty_symbols: Vec<SymbolRef>,
}

// crates/cgn-core/src/session/meta.rs
pub struct SessionMeta {
    // ... existing fields
    // NEW:
    pub watcher_pid: Option<u32>,
}

pub struct SymbolRef {
    pub name: String,
    pub kind: SymbolKind,    // function | method | struct | enum | trait | const | ...
    pub file: String,        // repo-relative
    pub line_start: u32,
    pub line_end: u32,
}
```

### 4.2 New (peer module)

```rust
// crates/cgn-core/src/peer/inbox.rs
pub enum InboxEntry {
    DirtyEvent {
        ts: String,
        peer_session: String,
        peer_pid: u32,
        kind: ConcernKind,           // Hard | Soft
        symbol: SymbolRef,
        reason: String,              // English, LLM-facing
        peer_delta: Option<String>,  // unified diff, populated for Hard only
        your_overlap_range: Option<(u32, u32)>,
    },
    Message {                         // Ƀ
        ts: String,
        msg_id: String,
        from: String,
        to: Option<String>,
        reply_to: Option<String>,
        body: String,
    },
}

pub enum ConcernKind { Hard, Soft }

// crates/cgn-core/src/peer/registry.rs
pub struct PeerSession {
    pub session_id: String,
    pub pid: u32,
    pub last_touched: chrono::DateTime<chrono::Utc>,
    pub base_sha: String,
    pub watcher_alive: bool,
}

pub fn alive_peers(repo_root: &Path, exclude_self: &str) -> Vec<PeerSession>;

// crates/cgn-core/src/peer/concern.rs
pub fn classify(
    peer_symbols: &[SymbolRef],
    my_dirty_symbols: &[SymbolRef],
    impact_cache: &ImpactCache,
) -> ConcernResult;

pub enum ConcernResult {
    Hard { symbol: SymbolRef, reason: String },
    Soft { symbol: SymbolRef, via_neighbor: SymbolRef, reason: String },
    Ignore,
}

// crates/cgn-core/src/peer/retention.rs
pub const MSG_LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;
pub const MSG_LOG_KEEP_ROTATED: usize = 7;
pub const WATCHER_LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
pub const WATCHER_LOG_KEEP_ROTATED: usize = 3;
pub const SESSION_STALE_DAYS: i64 = 30;
pub const ARCHIVE_PURGE_DAYS: i64 = 90;
pub const ROTATE_CHECK_EVERY_N_EVENTS: u32 = 100;
```

## 5. Write Path — Dirty Event Emission

Triggered by `post_tool_use` hook after any Edit / Write / NotebookEdit.

```
1. Read CLAUDE_CODE_SESSION_ID from env (existing resolver)
2. Resolve repo_root + session dir (existing registry path logic)
3. For each modified path:
   a. Run analyzer on the file (existing pipeline) → symbol table
   b. Diff against base_sha:
      - For each symbol whose AST range changed → add to dirty_symbols
   c. OverlayWriter::append_dirty(path, hash, fragment_id, dirty_symbols)
4. atomic_write_json(sessions/<me>/dirty.json)
   → rename(2) atomicity → mtime bump
   → peer watchers receive inotify IN_MODIFY
```

Cost: 5–20 ms per edit (dominated by analyzer parse). Already in hook path that includes other cgn operations; net addition is the symbol-diff step (~2 ms).

## 6. Read Path — Hook Drain

Triggered by `pre_tool_use` and `user_prompt_submit` hooks.

```
1. Resolve session dir from env
2. Read inbox.jsonl from last_drained_offset (stored in SessionMeta)
3. If no new entries:
   - emit nothing → hook returns silently
4. Else:
   - Parse entries
   - Render via peer::render::format():
     · HARD events: full peer_delta inline (capped at 30 LOC)
     · SOFT events: one-line summary, capped at 10 entries
     · Messages: full body (capped at 500 chars/msg)
   - Total payload capped at 4 KB
5. Atomic truncate inbox.jsonl + bump last_drained_offset
6. Emit:
   {
     "hookSpecificOutput": {
       "decision": "approve",
       "additionalContext": "<rendered payload>"
     }
   }
```

Cost: < 1 ms empty case; < 10 ms non-empty case.

## 7. Watcher Loop

Background process forked at session start (auto, opt-in via `<repo>/.cgn/auto-watch` marker file) or on demand (`cgn watch --start`).

### 7.1 Lifecycle

```
spawn:
  fork(); setsid(); redirect stdio → watcher.log
  flock(watcher.lock, LOCK_EX | LOCK_NB)
    → if fails: another watcher already running, exit
  write pid to SessionMeta.watcher_pid
  initialize impact_cache = IMPACT(my_dirty_symbols)

main loop:
  for event in inotify_stream:
    match event:
      IN_MODIFY on peer's dirty.json:
        peer_dirty = DirtyFiles::read(event.path)?
        for entry in peer_dirty.diff_since_last_seen():
          concern = classify(entry.dirty_symbols, my_dirty, &impact_cache)
          match concern:
            Hard | Soft => inbox.append(make_entry(concern, entry))
            Ignore       => skip
      IN_MODIFY on my own dirty.json:
        impact_cache.invalidate()   # recompute on next classify
      IN_Q_OVERFLOW:
        log warn; rescan all peer dirty.json

  every ROTATE_CHECK_EVERY_N_EVENTS:
    rotate_if_needed(msg.log, watcher.log)

shutdown:
  SIGTERM handler:
    release flock
    truncate inbox.jsonl
    exit(0)
```

### 7.2 Concern classification (precise definition)

```
Let MY_DIRTY_SYMBOLS = symbols in this session's dirty.json
Let PEER_SYMBOLS(e)  = symbols carried by peer event e
Let IMPACT(s)        = 1-hop graph neighbors of s
                       (callers, callees, HasMethod, HasProperty, Imports)

Hard   iff PEER_SYMBOLS(e) ∩ MY_DIRTY_SYMBOLS ≠ ∅
Soft   iff PEER_SYMBOLS(e) ∩ IMPACT(MY_DIRTY_SYMBOLS) ≠ ∅
                AND not already Hard
Ignore otherwise
```

`impact_cache` materializes `IMPACT(MY_DIRTY_SYMBOLS)` as a `HashSet<SymbolRef>` on watcher start and after every self-dirty change. Lookup is O(1) per peer symbol.

### 7.3 Symbol parse fallback

If analyzer cannot parse (unsupported language, syntax error, missing graph node), the entry's `dirty_symbols` is `[]`. Concern falls back to **file-level**: same file = Soft, else Ignore. Logged but does not block.

## 8. Message Path (Ƀ)

```
cgn peers say "body" [--to <peer>] [--reply <msg_id>]
  generate msg_id (uuid v7)
  build Message { from = me, to, reply_to, body }
  if to.is_some():
    append to sessions/<to>/inbox.jsonl
    append to my own msg.log with direction=sent
  else: (broadcast)
    for each alive_peer p:
      append to sessions/<p>/inbox.jsonl
    append once to my own msg.log with direction=sent, to=null

# Receiver side:
hook drains inbox → renders message section → also appends entry to msg.log with direction=recv
```

`msg.log` is the single source of truth for dialog history. Inbox is pure transport.

## 9. CLI Surface

```
cgn watch --start [--concern touched|all] [--foreground]
cgn watch --stop
cgn watch --status

cgn peers status                       # default toon
cgn peers diff <peer> [<symbol>]       # default text (unified diff)
cgn peers log [--since <duration>]     # tail msg.log
            [--peer <id>]
            [--direction sent|recv]
            [--limit N]
cgn peers say <text>  Ƀ                # fire-and-forget
            [--to <peer>]
            [--reply <msg_id>]
cgn peers inbox [--limit N]  Ƀ         # debug: read without drain
cgn peers thread <msg_id>    Ƀ         # debug: print thread (current session msg.log only)
cgn peers gc                           # rotate logs + archive stale sessions
```

Auto-spawn (opt-in): create empty `<repo>/.cgn/auto-watch` file; `session_start` hook spawns the watcher automatically.

### 9.1 MCP tools (extend `cgn-mcp` crate)

```
mcp__cgn__cgn_peers_status
mcp__cgn__cgn_peers_log
mcp__cgn__cgn_peers_say   Ƀ
```

These mirror CLI commands so an LLM can query peer state mid-conversation without shelling out.

## 10. Concurrency & Locking

| Resource | Mechanism | Rationale |
|---|---|---|
| `dirty.json` write | existing `atomic_write_json` (rename(2)) | Readers cannot observe partial state |
| `inbox.jsonl` append | `O_APPEND` open + single `write(2)` of one JSON line (always < PIPE_BUF=4096) | POSIX guarantees append atomicity at PIPE_BUF |
| `inbox.jsonl` drain + truncate | open with `O_RDWR`, read all bytes after offset, then `ftruncate(0)` + bump offset in SessionMeta | A concurrent append between read and truncate is lost only for that event; on subsequent fire the new offset detects file shrinkage → full re-read |
| `msg.log` append | `O_APPEND` per line | Same as inbox |
| `watcher.lock` | `flock(LOCK_EX | LOCK_NB)` | Kernel releases on process death; single instance per session |

## 11. Error Handling & Recovery

| Failure | Detection | Recovery |
|---|---|---|
| Watcher process dies, stale `watcher_pid` | Hook checks `/proc/<pid>` (Linux) / `kill(pid, 0)` (cross-platform) | `cgn peers status` shows `⚠ watcher dead`. Auto-watch mode re-spawns on next `session_start` |
| `inotify` queue overflow | `IN_Q_OVERFLOW` event | Log warn + full rescan of peer dirty.json files |
| `inbox.jsonl` partial JSON line | `serde_json::Error` during drain | Skip line, log warn. Should not occur given atomic append discipline |
| `inbox.jsonl` deleted externally | `ENOENT` on drain | Treat as empty inbox; continue |
| `last_drained_offset > file size` | Inbox shrank externally | Reset offset to 0, drain full file |
| Stale peer `dirty.json` (PID dead > 24 h) | Periodic check | `cgn peers status` shows `stale`; `cgn peers gc` archives |
| Analyzer parse failure on dirty file | `Result::Err` from analyzer | `dirty_symbols = []`, file-level fallback |
| Watcher main-loop panic | `std::panic::catch_unwind` wrapper around event handler | Log full backtrace to `watcher.log` (via `std::backtrace::Backtrace::capture()`), continue loop. **Fail-open.** |

### 11.1 Background task logging contract

All `Result::Err` and panic catches inside the watcher main loop **must**:

1. `tracing::error!(?err, "context")` with structured fields
2. Append full `Backtrace::capture()` rendering to `watcher.log`
3. NOT propagate — log and continue

Rationale: the watcher is daemonized; stdio is redirected to `watcher.log`. If the user's Claude session is doing other work, no console reads stderr. Without backtrace in the log, post-mortem debugging is impossible.

## 12. Log Rotation & Retention

| Log | Rotate trigger | Keep | Cap |
|---|---|---|---|
| `msg.log` | size ≥ 5 MB OR day boundary | 7 rotated | ~35 MB |
| `watcher.log` | size ≥ 10 MB | 3 rotated | ~30 MB |
| `inbox.jsonl` | drained → truncate (existing flow) | — | < 1 MB typical |
| `dirty.json` | live state, no rotation | — | < 100 KB typical |

Rotation mechanism: classic `mv` chain (`log.6 → log.7`, `log.5 → log.6`, …, `log → log.1`, new empty file).

Triggered at:
- `session_start` hook entry (cost: 2 × `fs::metadata().len()`)
- `cgn watch --start`
- Inside watcher loop every `ROTATE_CHECK_EVERY_N_EVENTS` (100)
- `cgn peers gc`

**Not** in hot path: per-message append, per-drain, per-classify. Hot paths assume rotation already handled.

Cross-session GC: `SessionMeta.last_touched > 30 days` → archive to `sessions/.archive/<id>-<date>/`. Archive entries > 90 days → delete. Performed only by explicit `cgn peers gc` or piggybacked on existing `cgn admin gc` (currently dead-code scaffolding; this PR does not wire it up, only reserves the interface).

## 13. Testing Strategy

### 13.1 Test Discipline (mandatory)

Every PR landing peer-sync changes must include three tiers, all green:

1. **Unit tests** — per-module logic (concern classification, inbox round-trip, rotation triggers)
2. **Integration tests** — multi-module flows with real filesystem (no mocks)
3. **Cross-session tests** — fork two processes simulating two Claude sessions

### 13.2 Test matrix

| Tier | File | Approx LOC | Coverage |
|---|---|---|---|
| Unit | `core/src/peer/concern.rs` `#[cfg(test)]` | 80 | HARD/SOFT/IGNORE, empty sets, asymmetric impact |
| Unit | `core/src/peer/inbox.rs` `#[cfg(test)]` | 50 | append, drain, watermark, partial-line tolerance |
| Unit | `core/src/peer/retention.rs` `#[cfg(test)]` | 40 | rotation trigger, keep count, filename chain |
| Integration | `cli/tests/peers_watch_lifecycle.rs` | 80 | `--start/--stop/--status`, flock race, stale detection |
| Integration | `cli/tests/peers_inbox_drain.rs` | 60 | hook stdin → drain → render → `hookSpecificOutput` |
| Integration | `cli/tests/peers_msg_log_rotation.rs` | 50 | actual rotate at threshold, old file deleted |
| Integration | `cli/tests/peers_watcher_log_backtrace.rs` | 40 | feed bad JSON, assert `watcher.log` contains backtrace |
| Cross-session | `cli/tests/peers_two_session_dirty_event.rs` | 80 | A writes dirty → B watcher receives < 500 ms → inbox entry |
| Cross-session | `cli/tests/peers_two_session_msg.rs` Ƀ | 60 | A `say` → B drain → body matches; reply reverse direction |
| Cross-session | `cli/tests/peers_symbol_level_filter.rs` | 80 | symbol-level filtering: same symbol = HARD, neighbor = SOFT, unrelated = IGNORE |
| Cross-session | `cli/tests/peers_concern_impact_cache_invalidation.rs` | 60 | self-dirty change invalidates impact cache |
| **Total** | | **~680** | |

### 13.3 Cross-session harness

```rust
// crates/cgn-cli/tests/common/peer_harness.rs (~120 LOC)
pub struct PeerHarness {
    repo_root: TempDir,
    sessions: Vec<SpawnedSession>,
}

impl PeerHarness {
    pub fn spawn_session(&mut self, id: &str) -> &SpawnedSession;
    pub fn dirty(&self, session: &str, path: &str, symbols: &[&str]);
    pub fn drain_inbox(&self, session: &str) -> Vec<InboxEntry>;
    pub fn say(&self, from: &str, to: Option<&str>, body: &str);
    pub fn assert_within(&self, timeout: Duration, predicate: impl Fn() -> bool);
}

impl Drop for PeerHarness {
    fn drop(&mut self) { /* reap all child watchers */ }
}
```

### 13.4 Anti-flakiness rules

| Risk | Mitigation |
|---|---|
| Inotify event delay | `assert_within(Duration::from_millis(500), ...)` poll loop, never blind `sleep` |
| Flock race | Test acquires lock first, asserts second `--start` is rejected |
| Watcher detaches after daemonize | Test uses `--foreground` mode, keeping stdout attached for assertion |
| Time-dependent rotation (day boundary) | Inject mockable `Clock` trait; test controls time directly |

### 13.5 Bench (non-blocking)

```
bench/peers_concern_classify.rs   # 1000 entries → < 5 ms total
bench/peers_inbox_drain.rs        # 100 entries → < 10 ms
```

Runs under existing `python scripts/benchmark_cgn.py --peers` flag.

## 14. Invariants

1. **No long-lived global daemon** — only per-session watchers, each forked at its session's start and reaped at its session's stop. The watcher *is* a daemonized process (forked + setsid'd), but its lifetime is strictly bounded by the parent Claude session.
2. **Watcher single-instance per session** — enforced by `flock` on `watcher.lock`.
3. **`inbox.jsonl` is drain-only** — never read without truncating.
4. **`msg.log` is append-only** — never edited or truncated outside rotation.
5. **`hookSpecificOutput` payload ≤ 4 KB** — hard cap; overflow drops oldest SOFT first, then trims peer_delta inline.
6. **No write to peer's directory other than `inbox.jsonl`** — message delivery never modifies peer's `dirty.json`, `meta.json`, or `msg.log`.
7. **All `Err` in watcher loop is logged + continued** — never propagated; never panics out of the loop.
8. **Symbol-level filtering uses live graph** — IMPACT is computed from current `ZeroCopyGraph`, not stale snapshots.
9. **Auto-watch opt-in only** — absence of `<repo>/.cgn/auto-watch` marker means no background process is spawned.

## 15. Out of Scope (v1)

| Item | Reason / Deferred to |
|---|---|
| Cross-repo peers | v1 limited to same `common_dir`. v2 may add `CGN_PEER_REPO=<path>` env |
| Explicit groups | User-rejected during brainstorming. Auto by `common_dir` is sufficient. Permanent decision. |
| `claim` / `release` coordination primitives | Introduces global locks + deadlock risk. Wait for concrete demand. |
| Daemon / socket transport | LLM has no event loop; low-latency advantage cannot reach the LLM |
| MCP push notification | Same reasoning — hook injection is the terminal path |
| Message encryption / auth | Single-user single-host assumption; OS-level `0700` is sufficient. v2 if multi-user emerges. |
| Rich text / attachments in messages | YAGNI; plain string body suffices |
| Web UI / TUI peer panel | Existing admin TUI is a v2 extension candidate, not this PR |
| Auto conflict resolution (merging peer delta) | Too dangerous; notify only, never modify |
| Message search index | `grep msg.log` is enough; indexing into Tantivy is over-engineered |
| Cross-host / network peers | Permanent no — cgn is local code intelligence, not a collab platform |
| Rotation config via CLI flag | v1 uses compile-time constants in `peer/retention.rs`; change at source |

## 16. File-Level Change Inventory

### 16.1 New files

| File | Approx LOC |
|---|---|
| `crates/cgn-core/src/peer/mod.rs` | 5 |
| `crates/cgn-core/src/peer/registry.rs` | 100 |
| `crates/cgn-core/src/peer/concern.rs` | 150 |
| `crates/cgn-core/src/peer/inbox.rs` | 120 |
| `crates/cgn-core/src/peer/retention.rs` | 80 |
| `crates/cgn-cli/src/peer/mod.rs` | 5 |
| `crates/cgn-cli/src/peer/watcher.rs` | 200 |
| `crates/cgn-cli/src/peer/dispatch.rs` | 80 |
| `crates/cgn-cli/src/peer/render.rs` | 150 |
| `crates/cgn-cli/src/commands/peers.rs` | 180 |
| `crates/cgn-cli/src/commands/watch.rs` | 120 |
| `crates/cgn-mcp/src/tools/peers.rs` | 60 |
| `crates/cgn-cli/tests/common/peer_harness.rs` | 120 |
| **Test files** (see §13.2) | 680 |
| **Subtotal new** | 1250 (impl) + 800 (tests + harness) = 2050 |

### 16.2 Modified files

| File | Δ LOC | Change |
|---|---|---|
| `crates/cgn-core/src/session/overlay.rs` | +30 | `DirtyEntry::dirty_symbols` field |
| `crates/cgn-core/src/session/overlay_writer.rs` | +40 | symbol extraction at write-time |
| `crates/cgn-core/src/session/meta.rs` | +5 | `watcher_pid` field |
| `crates/cgn-core/src/lib.rs` | +1 | `pub mod peer;` |
| `crates/cgn-cli/src/lib.rs` | +1 | `pub mod peer;` |
| `crates/cgn-cli/src/commands/hook/pre_tool_use.rs` | +60 | drain inbox + render + emit |
| `crates/cgn-cli/src/commands/hook/session_start.rs` | +40 | auto-watch opt-in spawn |
| `crates/cgn-cli/src/commands/hook/user_prompt_submit.rs` | +30 | drain inbox (secondary injection point) |
| `crates/cgn-cli/src/commands/hook/mod.rs` | +5 | register handlers |
| `crates/cgn-cli/src/commands/mod.rs` | +5 | wire `peers` + `watch` |
| `crates/cgn-cli/src/main.rs` | +15 | top-level dispatch |
| `crates/cgn-mcp/src/lib.rs` | +5 | register new tools |
| **Subtotal modified** | ~237 | |

### 16.3 Documentation / config

| File | Change |
|---|---|
| `~/.claude/SKILL.md` (cgn skill) | Add `peers` capability row |
| `CLAUDE.md` (project) | Note auto-watch marker file behavior |
| `docs/superpowers/specs/2026-05-17-multi-agent-peer-sync-design.md` | This file |

### 16.4 Grand total

| Category | LOC |
|---|---|
| New code (non-test) | 1250 |
| Modifications | 237 |
| Test files | 680 |
| Test harness | 120 |
| **Grand total** | **2287** |

In one PR (`feat/peer-sync`). Large but coherent — chosen explicitly during brainstorming over splitting into watch-PR + messaging-PR. Reviewer guidance: commit boundaries in §17 (deferred to plan phase) should follow analyzer → core peer → cli peer → watcher → CLI commands → hooks → MCP, each commit independently buildable.

## 17. Open Questions for Plan Phase

These are deferred to `writing-plans` rather than re-litigated here:

1. Exact phasing of the work into commit boundaries within the single PR (e.g. analyzer change → peer module → watcher → CLI → hooks → MCP).
2. Whether to ship the MCP tools in the same PR or as immediate follow-up.
3. Concrete commit messages and PR description structure.
4. Migration: existing sessions on `dirty.json` without `dirty_symbols` field — default deserialize to `[]` is sufficient (serde `#[serde(default)]`), no migration code needed.
