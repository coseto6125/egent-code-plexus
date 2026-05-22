# Follow-ups

Cross-session / cross-agent / cross-PR shared log of deferred work.
**Required reading before opening any PR; required updating before merge.**

---

## Protocol (for every agent and human working in this repo)

For every PR:

1. **Read** the `## Open` section first — check whether this PR resolves any
   existing entries.
2. **Resolved entries** — change the heading to `✅ done in PR #N` and move
   the entire entry into `## Done`. Do not delete.
3. **Newly surfaced deferrals** — append to the END of `## Open` using the
   template below. Never insert in the middle.
4. **PR description** must mention `Follow-ups: added FU-… / resolved FU-…`
   (the PR template enforces this with a checklist).

Append-only and status-marked. Old entries are kept; history is git-tracked.

### Entry template

```markdown
### FU-YYYY-MM-DD-NNN  ·  surfaced in PR #<n>
- **owner**: <github user | unassigned | session id>
- **scope**: <one sentence: what's deferred and why it matters>
- **why-deferred**: <out-of-scope | blocked-by | size-too-large | other>
- **next-action**: <concrete next step>
- **size**: <S | M | L | unknown>
- **links**: <related PRs / issues / commits / docs / memory files>
```

### ID format

`FU-{date}-{seq}` — `date` is the ISO date the PR is filed; `seq` is the
zero-padded daily sequence (`001`+). Sequences do not reset across PRs;
multiple PRs filed the same day continue the count.

### Status transitions

```
Open ──> [✅ done in PR #N]   ──> Done section
     ├─> [🚫 wontfix: reason]  ──> Done section (with rationale)
     └─> [⏸ blocked: <FU id>] ──> stays in Open; next-action notes the dep
```

`wontfix` entries are preserved (not deleted) so future maintainers see
the prior decision and can re-open from Done if context changes.

### Concurrent worktrees

`.gitattributes` sets `FOLLOWUPS.md merge=union`. Multiple worktrees may
append new entries to `## Open` in parallel — git auto-unions both
additions. Edits to the **same existing entry** still surface as a real
conflict; resolve by keeping the newer version and manually re-applying
the older change.

---

## Open

### FU-2026-05-22-001  ·  surfaced in PR #345
- **owner**: 另一條 session（dispatch indirection 5-phase roadmap 規劃中）
- **scope**: 12 種語言（TypeScript / JS / Java / Kotlin / CSharp / Go / Rust / PHP / Ruby / Swift / C++ / Dart）的 parser 沒有 Type 1 BlindSpot emitter；唯一 push BlindSpot 的是 `python/parser.rs:719`。等他們 PR 進來後，`ecp summary.blind_spots` 自動接到 dispatch-funcptr / vtable / callback / trait-object 等新 kind
- **why-deferred**: out-of-scope（屬另一條 session）+ size-too-large（~1700 LOC × 14 lang）
- **next-action**: 等對方 PR；本 session 不主動補 emitter；確認他們 CLI 用 `ecp summary --filter blind_spots.kind=dispatch-*` 而非新加 `ecp blind-spots` top-level
- **size**: L
- **links**: PR #345 commit `5e7cc4dd`；`crates/ecp-analyzer/src/python/parser.rs:703` (BLIND_SPEC reference)

### FU-2026-05-22-002  ·  surfaced in PR #345
- **owner**: unassigned
- **scope**: `dev::uid_audit::parse_hint` 用 `rsplit_once(':')` 切 name；若任一 parser 開始 emit 含 `:` 的 name（例如 Swift selector `init(foo:bar:)`），rsplit 會誤把 name 結尾的 `:` 當邊界
- **why-deferred**: 當前無 parser emit 此類 name，純理論風險
- **next-action**: 若未來 Swift parser 開始把 selector 寫進 uid-collision name 欄位，要把 hint 分隔符改成非 `:`（例如 `\x1F`），同時改 emit 端 `crates/ecp-analyzer/src/resolution/builder.rs:484` 與 parse 端
- **size**: S（改 1 行 emit + parse_hint，加 selector 測試）
- **links**: PR #345 commit `fix(dev/uid-audit): parse_hint preserves Rust ::`；`crates/ecp-cli/src/commands/dev/uid_audit.rs` parse_hint doc comment

### FU-2026-05-22-003  ·  surfaced in PR #345
- **owner**: unassigned
- **scope**: `ecp coverage` 與 `ecp group coverage` 別名（為一 release 向後相容）→ 一 release 後拔掉
- **why-deferred**: 等使用者 / docs / skill samples 完整切到 `summary` 命名後再移除
- **next-action**: 下一個 minor release 拔 `#[command(alias = "coverage")]` × 2 處（top-level Summary variant + group/mod.rs::Summary variant）+ 移除 `coverage_alias_still_routes_to_summary` 和 `group_coverage_alias_help_exits_zero` 兩個 back-compat test
- **size**: S
- **links**: PR #345；`crates/ecp-cli/src/main.rs` Summary variant；`crates/ecp-cli/src/commands/group/mod.rs` Summary variant

### FU-2026-05-22-004  ·  surfaced in PR #345
- **owner**: unassigned
- **scope**: `ecp admin verify-resolver` 別名（為一 release 向後相容）→ 一 release 後拔掉
- **why-deferred**: 同 FU-003，等切換完成
- **next-action**: 從 `crates/ecp-cli/src/commands/admin/mod.rs::AdminCommands` 移除 `VerifyResolver` variant + 對應 dispatch arm；保留 `ecp dev verify-resolver` 為唯一路徑
- **size**: S
- **links**: PR #345 commit `feat(cli-dev): hidden ecp dev namespace`

### FU-2026-05-22-005  ·  surfaced in PR #345
- **owner**: unassigned
- **scope**: `dev::uid_audit::build_report` 用 `sort_by_key + take(top)` 是 O(N log N) — 對 N=450k 級別大圖譜會慢；改用 `BinaryHeap<Reverse<_>>` 維持 size K 的 min-heap，達 O(N log K)
- **why-deferred**: 目前最大 sample (Go 449 records) 跑 < 20ms；效能未到瓶頸
- **next-action**: 等 corpus 規模上 10⁵ 級或 cold-ingest 後仍熱 path 才動；改完一定要加 benchmark（criterion 或 hyperfine）防退化
- **size**: S（~20 LOC）
- **links**: eywa hint `[tooling][algorithm] use heapq for top-K problems`；`crates/ecp-cli/src/commands/dev/uid_audit.rs::build_report`

### FU-2026-05-22-006  ·  surfaced in PR #345
- **owner**: unassigned
- **scope**: `.sample_repo/C` 索引失敗 — 缺檔 `deps/jemalloc/README`（submodule 沒同步）；本次多語言驗證跳過了 C
- **why-deferred**: 與 PR #345 scope 無關（parity sample 維護問題）
- **next-action**: `git submodule update --init --recursive` 在 `.sample_repo/C` 內跑一次；或在 oracle 同步腳本加 self-heal
- **size**: S
- **links**: PR #345 多語言驗證時 stderr 報錯

### FU-2026-05-22-007  ·  surfaced in PR #345
- **owner**: 另一條 session（dispatch indirection roadmap）
- **scope**: 另一條 session Phase 4 計畫加新 top-level command `ecp blind-spots --kind dispatch-...`；與本 session 的 `ecp summary.blind_spots` 是 section 而非 command 的決策衝突
- **why-deferred**: 需與對方協調，非單邊可定
- **next-action**: 等對方 PR 開出來；建議他們改用 `ecp summary --filter blind_spots.kind=dispatch-*` flag-based 而非新加 top-level；若對方堅持 top-level，再評估 alias 方案
- **size**: M（決策 + 2 邊改 ~50 LOC）
- **links**: PR #345 PR description 末段；`docs/vs-gitnexus.md`「honest unknown beats fabricated edge」

---

## Done

<!-- Move resolved or wontfix entries here. Keep their heading; prefix with
     `✅ done in PR #N` or `🚫 wontfix: <reason>`. Do not delete. -->
