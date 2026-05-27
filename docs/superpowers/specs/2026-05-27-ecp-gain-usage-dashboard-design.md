# `ecp gain` — Usage Dashboard Design

**Date**: 2026-05-27
**Status**: Design — pending implementation plan
**Topic**: A terminal usage dashboard for ecp (invocation counts, latency, errors), plus the CLI-path telemetry instrumentation that feeds it.

---

## 1. Problem & Goal

ecp records per-call telemetry **only for the MCP path** (`~/.ecp/telemetry/<repo>/calls.jsonl`, consumed by `ecp insight`). The **CLI path — the primary integration in Claude Code (see memory `project_ecp_cli_native_not_mcp`) — emits zero usage data.** Every `ecp inspect` / `find` / `impact` runs untracked: no invocation count, no latency record, and on failure only a `Command failed: {e}` print to stderr before exit (`main.rs:48-52, 197-200`). There is no error log.

**Goal**: a human-facing maintenance command `ecp gain` that surfaces, as a terminal ASCII dashboard:

1. **Usage counts** (primary) — how often each subcommand runs.
2. **Performance** (secondary) — latency vs the <30 ms budget.
3. **Errors** (the explicit user ask) — every command failure logged persistently, with a `--failures` drill-down.

It records **all events (success + failure)** so the three concerns derive from one data source.

### Non-goals (YAGNI)

- No HTML / TUI surface — terminal ASCII only.
- No color theme system — auto-detect + `NO_COLOR` standard only.
- Not an LLM context-building tool. `ecp gain` is for the **human maintainer**, like `ecp admin doctor`. It stays out of every hot path.

---

## 2. Positioning

- **Top-level visible command** `ecp gain`, sibling to `ecp insight`.
- **Audience**: human maintainer. The LLM-utility A/B/C gate does not apply (this is a maintenance command, not graph schema). Documented as such.
- **Telemetry default-on, opt-out**: data is local-only (`~/.ecp/`, never transmitted, same as MCP telemetry). A `NO_COLOR`-style off switch (`ECP_NO_TELEMETRY=1` or config `telemetry.cli = false`) respects user control.

---

## 3. Architecture

Three layers; only **collection** and **presentation** are new — **aggregation reuses `insight`**.

```
┌─ Collection (NEW — zero hot-path cost) ────────────────────┐
│  main.rs dispatch wrapped in a timer                        │
│    ok   → CallRecord{ok:true, ...}                          │
│    err  → CallRecord{ok:false, error_kind}                  │
│  best-effort append (no flock; single-line atomic write)    │
│        ↓                                                     │
│  ~/.ecp/telemetry/<repo>/cli-calls.jsonl                    │
│  (same dir + same schema as MCP calls.jsonl, +2 fields)     │
└─────────────────────────────────────────────────────────────┘
                       ↓ read
┌─ Aggregation (REUSE insight.rs: p50/p99/error_rate) ────────┐
│  read cli-calls.jsonl + calls.jsonl (MCP), merge & aggregate │
└─────────────────────────────────────────────────────────────┘
                       ↓
┌─ Presentation (NEW) — ecp gain ─────────────────────────────┐
│  ASCII dashboard: Usage > Performance > Errors              │
│  flags: -p (this repo) · --failures · --all · --format json │
└─────────────────────────────────────────────────────────────┘
```

### 3.1 Data model — extend the shared `CallRecord`

`CallRecord` (`crates/ecp-mcp/src/telemetry.rs:29`) gains two append-only fields so CLI and MCP share one schema and one directory; `ecp gain` reads both, `ecp insight` keeps working:

```rust
pub struct CallRecord<'a> {
    pub ts: &'a str,
    pub tool: &'a str,                // CLI: subcommand name ("inspect"); MCP: "ecp_inspect"
    pub duration_ms: u64,
    pub ok: bool,
    pub source: &'a str,              // NEW: "cli" | "mcp"
    pub error_kind: Option<&'a str>,  // NEW: failure class, only when ok == false
}
```

- MCP writer (`server.rs:50-65`) sets `source:"mcp", error_kind:None`.
- Reading old records lacking the new fields: serde `#[serde(default)]` → `source` defaults to `"mcp"` (existing files are MCP-only), `error_kind` to `None`. Backward compatible.
- The telemetry module likely moves/shares between `ecp-mcp` and a place reachable from `ecp-cli` (e.g. lift `CallRecord` + `append` into `ecp-core`, or expose via `ecp-mcp`'s public API). The implementation plan resolves the exact crate boundary; the constraint is **one struct, not two**.

### 3.2 Collection — hot-path safety (the load-bearing constraint)

CLAUDE.md priority #1 is per-query latency <30 ms. Therefore:

- **No synchronous fsync in dispatch.** Wrap `match cli.command` in `Instant::now()`, build the record after, and write **best-effort** (failure to write never affects the command's own result/exit code).
- **Append mode, no flock.** A single JSONL line is `< PIPE_BUF` (4096 on Linux) so `O_APPEND` writes are atomic under POSIX without locking. This deliberately avoids the lock-contention class that caused the registry deadlock (memory `project_ecp_registry_lock_deadlock`). The MCP writer (`telemetry.rs:88`) already follows this lock-free append pattern — mirror it.
- **Skip when opted out** (`ECP_NO_TELEMETRY=1` / config) — early return before any I/O.
- The pre-graph-load fast path (`main.rs:67-85`) and the normal dispatch (`main.rs:168-196`) both route through the timer wrapper so no subcommand is silently untracked.

### 3.3 Error taxonomy (`error_kind`)

On the failure path (`main.rs` error arm), classify `Err(e)` into a small closed set before writing. Initial taxonomy:

| `error_kind`        | Trigger |
|---------------------|---------|
| `cypher-parse`      | Cypher query parse/label/syntax error |
| `no-such-symbol`    | target symbol not found (find / impact / rename) |
| `index-stale`       | graph.bin older than HEAD; reindex spawned |
| `graph-load-failed` | graph.bin missing / corrupt / registry lock timeout |
| `other`             | unclassified (carries the raw message) |

Classification lives at the dispatch boundary, mapping the error type/variant to a kind. Unknown → `other` with the raw message preserved for `--failures`.

---

## 4. Presentation (the screens)

Section order follows the user's priority: **Usage → Performance → Errors.**

### 4.1 `ecp gain` (default)

```
ecp Usage Dashboard                                    repo: code-graph-nexus
════════════════════════════════════════════════════════════════════════════
 Total invocations  12,847        Error rate  2.3%  (296 failed)
 Tracked since      2026-04-12    Sources     cli 11,203 · mcp 1,644
 Median latency     8 ms          p99 latency 142 ms

▌Usage  (by subcommand)
────────────────────────────────────────────────────────────────────────────
  #  Command       Count    Share   p50     p99    Err%   Trend
────────────────────────────────────────────────────────────────────────────
  1  inspect       4,210    32.8%    6ms    48ms   0.4%   ████████████ ▁▃▅█
  2  find          3,102    24.1%    4ms    31ms   0.9%   █████████░░░ ▂▄▆█
  3  impact        1,876    14.6%   12ms   142ms   1.2%   ██████░░░░░░ ▅▃▂▁
  4  cypher        1,344    10.5%    9ms    88ms   6.1% ! ████░░░░░░░░ ▁▂▅█
  …  (more — see --all)
────────────────────────────────────────────────────────────────────────────

▌Performance  (latency budget: <30ms target)
────────────────────────────────────────────────────────────────────────────
  Within budget   ██████████████████████░░  91.2%   (11,716)
  Over 30ms       ██░░░░░░░░░░░░░░░░░░░░░░░   8.8%   (1,131)
  Slowest:  impact 142ms p99 · rename 210ms p99

▌Errors  (296 total · 2.3%)
────────────────────────────────────────────────────────────────────────────
  cypher-parse        118  ████████████░░░░  ← run with --failures
  no-such-symbol       74  ███████░░░░░░░░░
  index-stale          52  █████░░░░░░░░░░░
  other                21  ██░░░░░░░░░░░░░░
────────────────────────────────────────────────────────────────────────────
  Tip: ecp gain --failures   for recent failing commands + messages
```

- Top three-column summary = at-a-glance state.
- `!` marks commands with anomalously high error rate.
- `Trend` sparkline (`▁▃▅█`) = recent per-day bucket. **Cost note**: requires daily bucketing (like insight's `hourly_buckets`). Kept — it is the strongest "dashboard feel" element and reuses insight's bucketing approach. Degrades gracefully to blank when <2 days of data.

### 4.2 `ecp gain --failures`

```
ecp Failures  (recent 20 of 296)                       repo: code-graph-nexus
════════════════════════════════════════════════════════════════════════════
 05-27 07:41  cypher        cypher-parse      "MATCH (n:Functon) RETURN n"
              └ Unknown label 'Functon' at col 9 — did you mean 'Function'?
 05-27 07:38  impact        no-such-symbol    impact --target paese_fil
              └ No symbol 'paese_fil' found (closest: parse_file, 0.82)
 05-27 06:55  find          index-stale       find handleAuth
              └ graph.bin older than HEAD; auto-reindex spawned, retry
────────────────────────────────────────────────────────────────────────────
  Full log: ~/.ecp/telemetry/code-graph-nexus/cli-calls.jsonl
```

Pure scan of `ok:false` records — no extra storage. Each entry: ts · command · error_kind · args summary · actual message.

### 4.3 `ecp gain --format json` (program/agent consumption — color always off)

```json
{
  "repo": "code-graph-nexus",
  "total": 12847,
  "error_rate": 0.023,
  "by_command": [
    {"cmd":"inspect","count":4210,"p50_ms":6,"p99_ms":48,"err_rate":0.004},
    {"cmd":"cypher","count":1344,"p50_ms":9,"p99_ms":88,"err_rate":0.061}
  ],
  "errors_by_kind": {"cypher-parse":118,"no-such-symbol":74,"index-stale":52},
  "within_budget_pct": 0.912
}
```

### 4.4 Flags

| Flag | Effect |
|------|--------|
| (none) | default dashboard, scope = all repos with telemetry |
| `-p`, `--project` | scope to current repo only |
| `--failures` | recent failing commands + messages |
| `--all` | full per-subcommand table (no truncation) |
| `--format <text\|json>` | default `text`; `json`/`csv` force color off |
| `--no-color` | force color off |

---

## 5. Color

rtk's `gain` has no color flag and emits plain text when piped (verified: `█░` are Unicode blocks, not ANSI). `ecp gain` goes slightly stricter, because the project's primary consumer is an agent and ANSI escapes pollute context.

**Three-state detection, in order:**

```
1. --no-color OR NO_COLOR env set            → no color (highest priority)
2. --format json / csv                        → no color (structured output)
3. stdout is not a TTY (piped / captured)     → no color (auto)
4. otherwise (human at a terminal)            → color on
```

- Use `std::io::IsTerminal` (stdlib, zero-dep — per "highest-level stdlib" principle). No `colored`/`owo-colors` crate; define a few ANSI consts inline.
- Color **reinforces existing signal, adds no new dimension**: the `!` high-error marker turns red; `within budget` green / `over 30ms` yellow→red; sparkline tracks trend direction.
- No theme system (YAGNI for a maintenance command).

---

## 6. Performance

- **Query hot path: ~zero.** Best-effort background-style append, no synchronous fsync, no flock, opt-out short-circuits before I/O.
- **Verification (required, not assumed)**: run `scripts/benchmark/benchmark_ecp.py` with telemetry on vs off; assert p50 does not regress. CLAUDE.md requires profiling evidence for perf claims — no claim ships without it.
- **Disk growth / retention**: ~100 bytes/line. Bounded by **time-based retention, not size rotation** — time aligns with the dashboard's per-day sparkline window (a fixed 7-day window means the chart shows the whole file, no "graph ends yesterday but file has last week's residue" mismatch), and gives predictable disk (7 days × usage, naturally capped at a few MB).
  - **Window**: configurable `telemetry.retention_days` (default `7`). Not hardcoded (per `feedback_generality_no_hardcode`).
  - **Pruning timing — the hot-path rule**: append is **always append-only**; pruning is NEVER done on the write path (read-whole-file → filter → rewrite is O(n) I/O + lock, would violate <30 ms — same trap as `project_ecp_registry_lock_deadlock`). Instead:
    - **Primary**: when `ecp gain` itself runs (it already reads the whole file; the reader is the human maintainer, off the hot path) — drop lines older than the window and rewrite once.
    - **Fallback**: hook into the existing `ecp admin gc` / `prune` background flow (the established `flock -n` pattern). `gc.rs:44` currently exempts the whole `telemetry/` dir — change to: keep exempting MCP `calls.jsonl`, but apply `retention_days` pruning to `cli-calls.jsonl`.
  - **Rule of thumb**: the writer never prunes; the reader and GC prune.

---

## 7. Testing

`ecp gain` is CLI-surface, single-language (Rust internal), so the 14-language parity rule does **not** apply (it touches no parser / graph-construction primitive).

- **Collection**: a command runs → exactly one well-formed JSONL line appended with correct `ok` and `source:"cli"`; a failing command → `ok:false` with the expected `error_kind`. Opt-out → no line written.
- **Hot-path**: telemetry-on vs off latency delta within noise (benchmark assertion).
- **Aggregation**: synthetic jsonl fixture → known counts / p50 / p99 / error_rate; merges MCP + CLI lines correctly; old records (missing new fields) parse via serde defaults.
- **Presentation**: golden-output test for the default dashboard, `--failures`, `--format json`. Color: non-TTY / `NO_COLOR` / `--no-color` produce byte-identical plain text (assert no `\x1b[`).
- **Error taxonomy**: each `error_kind` trigger maps to the right class; unknown → `other` preserving the raw message.

---

## 8. Open decisions for the plan

- Exact crate home for the shared `CallRecord` + `append` (lift to `ecp-core` vs expose from `ecp-mcp`). Constraint: one struct.
- Whether `source` distinction warrants a top-level cli/mcp split row in the Usage table or stays in the summary line (current design: summary line only).
