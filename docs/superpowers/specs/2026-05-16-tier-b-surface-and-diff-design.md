# Tier B: CLI surface 整理 + `gnx diff` 泛化 diff 命令

**Date**: 2026-05-16
**Status**: Design (spec) — pre-implementation
**Goal**: 將既有但 hidden 的 `mcp` / `shape_check` / `verify_resolver` 三個 command 重新定位到正確的 CLI 層級；同時為 LLM agent 引入新的 `gnx diff` 命令，對任意兩個 git ref 之間的 graph 結構差異提供結構化輸出。

**Related**:
- 既有 `crates/graph-nexus-cli/src/commands/{mcp,shape_check,verify_resolver}.rs`
- `crates/graph-nexus-cli/src/main.rs` 的 `Commands` enum dispatch
- `~/.claude/skills/gnx/SKILL.md` Tool selection table
- 既有 `gnx impact --since <ref>` 做為 reference 對照

---

## 1. Problem statement

### 1.1 Hidden commands surface 落差

`crates/graph-nexus-cli/src/commands/` 內已實作三個 command，但 `gnx --help` 都沒列：
- `mcp.rs`（PR #11 引入）— MCP server 啟動 + tool 列舉
- `shape_check.rs` — HTTP consumer ↔ Route 的 drift detector，LLM 面向設計
- `verify_resolver.rs` — Resolver decision 跟 oracle JSONL 對照的 benchmark 工具

結果：
- 用戶 / agent 不知道有這些功能（discoverability 0）
- skill doc 沒列，agent 不會主動呼叫
- MCP tool 自動 list 對 hidden command 行為不一致

### 1.2 LLM agent 缺「graph-level PR diff」工具

當 agent 在 PR review 場景想知道「這次 commit 對 graph 有什麼結構性影響」時，現有工具不足：
- `git diff` 只看 source 文字級差異，看不出 graph 結構變化
- `gnx impact --since <ref>` 是 「source change → 上下游 blast radius」單向 query，不給「整個 graph 的對比視角」
- `verify_resolver --oracle` 需要外部 LSP 跑出來的 oracle.jsonl，agent / 一般 dev 拿不到 → 等同對 agent 不可用

**agent 在 PR review / pre-merge 流程實際需要**：
- 「跑 baseline vs current 兩次 resolver dump，告訴我有哪些 symbol resolution 變了」
- 「哪些 routes 加減」
- 「哪些 cross-repo contracts schema 改了」

這三類「graph 結構元素的 diff」就是新 `gnx diff` 要解決的問題。

---

## 2. Scope

### 2.1 In scope

| 動作 | 對象 |
|---|---|
| Move `mcp` to admin namespace | `gnx admin mcp serve\|tools` |
| Move `verify_resolver` to admin namespace | `gnx admin verify-resolver --oracle X --gnx Y --lang Z` |
| Un-hide `shape_check` to top-level | `gnx shape_check --route <path>?` |
| Add `--route <path>` arg to `shape_check` | 過濾單一 route 的 drift |
| Add `--format` arg to `mcp tools` | `json\|toon\|text` |
| New `gnx diff --section <bindings\|routes\|contracts\|all> --baseline <ref>` | 泛化 graph diff |
| Update `~/.claude/skills/gnx/SKILL.md` Tool selection table | 加 `shape_check` 升級版 + `diff` 新 row |
| Tests covering all dispatch paths + new args | `crates/graph-nexus-cli/tests/` |

### 2.2 Out of scope（明示延後 / 不做）

- `gnx diff --section symbols`：source-level symbol 加減（git diff 已能拿大部分；可未來加）
- `gnx diff --section edges`：edge-level diff（噪音太多，需先想 group/filter 策略，獨立 spec）
- `verify_resolver --baseline <ref>` self-baseline mode：先把 verify_resolver 移到 admin，self-baseline 由 `gnx diff --section bindings` 承擔，無需 verify_resolver 自己加 mode
- `--auto-oracle` LSP integration：跨語言 LSP adapter 是長期工程，獨立 spec
- 「agent narrative output」：gnx 不自帶 narrative formatting，JSON / text 已足；narration 由 agent 解讀 JSON 後自做

### 2.3 Verify_resolver 為何不升 top-level

技術上 `verify_resolver` 需要外部 oracle JSONL（pyright / tsc / rust-analyzer dump）作為 ground truth。短期內無 LSP 整合，agent 拿不到 oracle → 即使 surface 為 top-level，agent 也無法呼叫成功。Self-baseline 模式（用 git ref 作為 baseline）改由新 `gnx diff --section bindings` 涵蓋，職責分離：
- `gnx diff --section bindings`：agent 用，gnx 自己對比兩 commit
- `gnx admin verify-resolver`：dev 用，跟外部 LSP oracle 對比

---

## 3. Approach

採用 **Approach A: 純 visibility flip + namespace move + 補 args + 新 diff command**。

理由：
- 三個 hidden command 已有完整實作，主要工作是 visibility 跟 dispatch 路徑
- `gnx diff` 是新加但邊界清楚（3 個 section + baseline resolution + output 格式）
- 不動 graph storage / parser / resolver tier 內部邏輯
- 風險低、tests 容易覆蓋

替代方案（已 rejected）：
- B. surface + 大幅補 args（如 `mcp serve --port`、`shape_check --consumer <file>` 等）：scope creep，獨立用例可後加
- C. 把 `gnx diff` 做成全面摘要單一 command：agent ROI 評估顯示「全面摘要 30% 有用 70% 噪音」，拆成 `--section` 後讓 agent 按 task 選

---

## 4. CLI surface 與 dispatch

### 4.1 Before vs After

```
Before                              After
──────                              ─────
gnx                                 gnx
├── (9 visible agent commands)      ├── (9 visible agent commands)
│   ├── inspect                     │   ├── inspect
│   ├── search                      │   ├── search
│   ├── impact                      │   ├── impact
│   ├── rename                      │   ├── rename
│   ├── cypher                      │   ├── cypher
│   ├── coverage                    │   ├── coverage
│   ├── routes                      │   ├── routes
│   ├── scan                        │   ├── scan
│   └── contracts                   │   └── contracts
├── (hidden) mcp                    ├── shape_check                  ← NEW visible
├── (hidden) shape_check            ├── diff                          ← NEW command
├── (hidden) verify_resolver        └── admin
└── admin                                ├── install-hook
    ├── install-hook                     ├── (...)
    ├── (...)                            ├── mcp serve|tools          ← MOVED from top-level
    ├── config                           └── verify-resolver          ← MOVED from top-level
    ├── group
    └── index
```

### 4.2 Visibility 規則

| Command | 位置 | 對 agent 是否 mcp tool list 暴露 | 理由 |
|---|---|---|---|
| `inspect / search / impact / rename / cypher / coverage / routes / scan / contracts` | top-level visible | ✅ 已是 | agent 高頻自動呼叫 |
| `shape_check` | top-level visible | ✅ 加入（drift detector，agent 在 API task 自動會用） | LLM-facing 文案、context cost ~200 tokens 換高 ROI |
| `diff` | top-level visible | ✅ 加入 | agent PR-review / pre-merge 流程必用 |
| `admin mcp` (serve/tools) | admin namespace | ❌（其本身是 MCP entry，自暴自己無意義） | 由 MCP host spawn；user 偶爾手動測 |
| `admin verify-resolver` | admin namespace | ❌（agent 拿不到 oracle） | Dev benchmark tool |
| 其他 admin commands | admin namespace | ❌（user maintenance） | install-hook / drop / prune 等 |

### 4.3 新增 args

| Command | New arg | Type | Behavior |
|---|---|---|---|
| `shape_check` | `--route <path>` | `Option<String>` | 過濾 target Route's path 匹配 `<path>` 的 Fetches edges；None = 全跑 |
| `admin mcp tools` | `--format <json\|toon\|text>` | `OutputFormat` | 對齊 gnx 其他 command 慣例；default `text` |
| `diff` | `--section <s>` | required | 接受 `bindings` / `routes` / `contracts` / `all` 或 `,` 分隔多選 |
| `diff` | `--baseline <ref>` | required, no default | git ref（branch / tag / SHA / `HEAD~N` / `PR/<n>`）|
| `diff` | `--format <json\|toon\|text>` | `OutputFormat` | default `text` |
| `diff` | `--verbose` | `bool` | text format 是否列全部變化（預設 truncate top-N） |

---

## 5. `gnx diff` 詳細設計

### 5.1 命令樣態

```bash
# 必填: --section + --baseline
gnx diff --section bindings --baseline origin/main
gnx diff --section routes --baseline v1.2.0
gnx diff --section contracts --baseline a8b2f54
gnx diff --section all --baseline PR/13

# 多選 section
gnx diff --section bindings,routes --baseline origin/main

# 輸出格式
gnx diff --section all --baseline origin/main --format json
gnx diff --section all --baseline origin/main --format toon

# 詳細模式（text）
gnx diff --section all --baseline origin/main --verbose
```

不給 `--baseline` 直接 reject，不 fallback 預設值（防止 silent drift）。

### 5.2 Baseline ref 解析

`<ref>` 接受形式：

| 格式 | 範例 | 內部解析 |
|---|---|---|
| Branch | `main`, `origin/main` | `git rev-parse <ref>` → SHA |
| Tag | `v1.2.0` | 同上 |
| Commit SHA | `a8b2f54`, full SHA | 同上 |
| Relative ref | `HEAD~5`, `HEAD@{1.day.ago}` | 同上 |
| PR number | `PR/13` | `gh pr view 13 --json baseRefOid` → SHA |
| 預設 | (無) | **reject with error**，不給預設 |

GitHub-only：`PR/<n>` 透過 `gh` CLI（已是 gnx 環境慣例）。GitLab / Gitea / Bitbucket 不支援，user 須自己傳 SHA。錯誤訊息提示用法。

### 5.3 內部 data flow

```
gnx diff --section bindings --baseline PR/13
  ↓
1. resolve baseline ref → SHA (e.g. "a8b2f54")
   - branch/tag/sha/relative → git rev-parse <ref>
   - PR/<n> → gh pr view <n> --json baseRefOid
   - 失敗 → error + 列接受格式
2. ensure working tree clean
   - dirty → git stash push (auto-stash, 結尾 stash pop)
3. checkout baseline SHA in detached HEAD
4. gnx index + dump section data → /tmp/gnx-diff-baseline-<sha>.jsonl
5. checkout 回原 branch / commit
6. gnx index + dump section data → /tmp/gnx-diff-current-<sha>.jsonl
7. diff baseline vs current per section
8. emit text/json/toon
9. cleanup tmp files
10. git stash pop (if stashed in step 2)
```

**重要保證**：
- 步驟 9-10 用 RAII / defer pattern 確保 finally 執行，即使中途失敗也還原 git 狀態
- baseline graph 用獨立 `~/.gnx/graph-nexus-rs/<branch-slug>-baseline-<sha>/graph.bin` 路徑，避免覆蓋當前 working graph

### 5.4 Section 內容定義

#### 5.4.1 `bindings` section

對每個 `(src_file, symbol_name)` binding pair 比對 baseline vs current 的 resolver decision：

```
─ Section: bindings ─
new_resolutions:  N           # baseline Unresolved, current resolved
tier_changes:     N           # tier 不同
target_changes:   N           # target file 不同
removed:          N           # baseline resolved, current Unresolved

[NEW] app/lib/forwardable.rb:8 :: read
   was: Unresolved (BlindSpot)
   now: app/lib/forwardable.rb:8 :: read (HeritageScoped, 0.80)

[TIER↑] crates/foo.rs:289 :: helper
   was: Global (0.70)
   now: ImportScoped (0.95)

[TARGET] some/file.py :: bar
   was: pkg/old.py:42 (ImportScoped)
   now: pkg/new.py:78 (ImportScoped)
```

實作上對應 [[ruby-receiver-aware-resolver]] PR #13 引入的 `--dump-resolver` 機制（同 verify_resolver 用的 dump 格式）。

#### 5.4.2 `routes` section

對每個 `(method, path)` route pair 比對：

```
─ Section: routes ─
added:    N
removed:  N
modified: N           # 同 path 但 handler / response shape 改

[ADDED]   GET  /api/users/{id}/posts → app/api/users.py:handler_posts
[REMOVED] POST /api/legacy/login     → app/legacy/auth.py:legacy_login
[MODIFIED] PUT /api/users/{id}
   handler unchanged: app/api/users.py:update_user
   response_shape: +"updated_at"
```

對應 `gnx routes` 已有的 RouteShape 抽取。

#### 5.4.3 `contracts` section

對 cross-repo contracts（RPC / queue / fetch shape）比對：

```
─ Section: contracts ─
added:    N
removed:  N
modified: N           # signature / schema 改

[MODIFIED] POST /api/payment (consumer: payments-frontend)
   request_shape:  +"currency", -"deprecated_field"
   response_shape: unchanged

[MODIFIED] user.created queue (producer: auth-service)
   payload:        +"tenant_id"
```

對應 `gnx contracts` 已有的 cross-repo inventory。

#### 5.4.4 `all` section

跑 `bindings + routes + contracts` 三個 section 並合併 output。等同 `--section bindings,routes,contracts`。

### 5.5 Output format

#### 5.5.1 text（default）

人類可讀，含 section header + 統計摘要 + per-change 詳情。預設 truncate per section 最多 10 changes（top-N by significance）；`--verbose` 列全部。

#### 5.5.2 json

機器可解析，agent narration 用：

```json
{
  "baseline": {"ref": "origin/main", "sha": "a8b2f54"},
  "current": {"ref": "HEAD", "sha": "d9ae1be"},
  "sections": {
    "bindings": {
      "new_resolutions": [...],
      "tier_changes": [...],
      "target_changes": [...],
      "removed": [...]
    },
    "routes": {
      "added": [...],
      "removed": [...],
      "modified": [...]
    },
    "contracts": {
      "added": [...],
      "removed": [...],
      "modified": [...]
    }
  }
}
```

#### 5.5.3 toon

緊湊版 key:value 格式，agent → agent piping 用。

### 5.6 Exit code

- `0` always when diff runs to completion（無論是否有 changes）— diff 是 advisory，不該 fail-hard
- `1` 僅當 dispatch / baseline resolve / git ops 失敗
- 未來 `--strict` flag 可加：「視 changes 數超閾值為 fail criteria」交給 CI 用

---

## 6. Components / file-level changes

| File | Change |
|---|---|
| `crates/graph-nexus-cli/src/main.rs` | `Commands` enum 移除 `Mcp` / `VerifyResolver` variants；移除 `ShapeCheck` 的 `hide` 屬性；新增 `Diff(commands::diff::DiffArgs)` variant + dispatch |
| `crates/graph-nexus-cli/src/admin/mod.rs` | `AdminCommand` enum 加 `Mcp(McpArgs)` + `VerifyResolver(VerifyResolverArgs)` variants；dispatch 分支 |
| `crates/graph-nexus-cli/src/commands/mcp.rs` | `McpAction::Tools` 加 `--format <OutputFormat>` arg；run() 按 format 輸出 |
| `crates/graph-nexus-cli/src/commands/shape_check.rs` | `ShapeCheckArgs` 加 `--route <Option<String>>`；run() 過濾 Fetches edges by target route path |
| `crates/graph-nexus-cli/src/commands/verify_resolver.rs` | 不變（內部 logic 保留，僅 dispatch 移到 admin） |
| **新** `crates/graph-nexus-cli/src/commands/diff/mod.rs` | `DiffArgs` 定義 + dispatch entry |
| **新** `crates/graph-nexus-cli/src/commands/diff/baseline.rs` | Baseline ref 解析（git rev-parse / gh pr view）+ git stash/checkout/restore RAII helper |
| **新** `crates/graph-nexus-cli/src/commands/diff/bindings.rs` | bindings section diff logic |
| **新** `crates/graph-nexus-cli/src/commands/diff/routes.rs` | routes section diff logic |
| **新** `crates/graph-nexus-cli/src/commands/diff/contracts.rs` | contracts section diff logic |
| **新** `crates/graph-nexus-cli/src/commands/diff/output.rs` | text / json / toon formatters |
| `~/.claude/skills/gnx/SKILL.md` | Tool selection table 加 `shape_check`（含 `--route`）+ `diff`（含 sections + baseline）2 rows；admin 區段補 `mcp` / `verify-resolver` 說明 |
| `crates/graph-nexus-cli/tests/admin_mcp_test.rs` | 驗 `admin mcp tools --format toon` 走通；`admin mcp serve` 啟動 stdio |
| `crates/graph-nexus-cli/tests/admin_verify_resolver_test.rs` | 驗 `admin verify-resolver` dispatch |
| `crates/graph-nexus-cli/tests/shape_check_route_filter.rs` | 驗 `--route /api/foo` 過濾正確；無匹配 route 提示 |
| `crates/graph-nexus-cli/tests/diff_bindings_test.rs` | 各種 baseline ref 形式（branch/tag/SHA/HEAD~N/PR）解析；bindings section diff output |
| `crates/graph-nexus-cli/tests/diff_routes_test.rs` | routes section 加減改 |
| `crates/graph-nexus-cli/tests/diff_contracts_test.rs` | contracts section schema diff |
| `crates/graph-nexus-cli/tests/diff_all_section_test.rs` | `--section all` 跟 `--section bindings,routes,contracts` 結果一致 |
| `crates/graph-nexus-cli/tests/cli_help_surface_test.rs` | snapshot：`gnx --help` 含 `shape_check` / `diff` 但不含 `mcp` / `verify-resolver`；`admin --help` 含這兩個 |

---

## 7. Error handling

| Scenario | Behavior |
|---|---|
| `gnx diff` 不給 `--baseline` | clap reject + 列接受 ref 格式範例 |
| `--baseline <ref>` 解析失敗 | `error: cannot resolve <ref>: <git error>` + hint 列範例 |
| `gh` CLI 缺（用 `PR/<n>` 時） | `error: gh CLI not found; install gh or pass commit SHA directly` |
| Baseline commit 本地沒 | 嘗試 `git fetch` 一次；仍沒 → error 含 `git fetch <remote>` hint |
| 工作樹髒 | auto `git stash push -u`，diff 結束 `git stash pop`；finally semantics 確保還原 |
| Diff 中途 panic / signal | finally cleanup hook：checkout 回原 branch + stash pop + 刪 tmp files |
| `--section <invalid>` | clap reject + 列 possible values |
| `shape_check --route <path>` 無匹配 | text 輸出 `No routes match '<path>'`，exit 0 |
| `admin mcp tools --format <invalid>` | clap reject + possible values |

新加 error variants（如需）：`GnxError::BaselineResolve(String)`, `GnxError::GitOp(String)`。對齊既有 thiserror pattern。

---

## 8. Testing

### 8.1 Test 範圍

每個新動作 / arg 配對至少一個 integration test，總計約 7 個新 test 檔（見 §6 表）。

關鍵 test case：
- `diff --baseline` 各種 ref 形式都能解析
- `diff --baseline` 解析失敗時錯誤訊息含 hint
- `diff` 中途模擬 panic → 工作樹仍還原
- `diff --section all` 跟逐 section 跑結果一致
- `admin mcp tools --format toon` 輸出格式對齊其他 command
- `shape_check --route` 過濾正確 + 無匹配時 graceful
- `gnx --help` snapshot 對應 visibility 規則
- `admin --help` snapshot 對應 namespace 規則

### 8.2 CI 整合

既有 `.github/workflows/ci.yml` 跑 `cargo nextest run --workspace`，自動涵蓋新 test 檔；無需新增 workflow step。

`cargo clippy --workspace --all-targets -- -D warnings` 涵蓋新檔 lint。

不引入 perf benchmark CI gate（baseline diff 慢度待後續觀察）。

---

## 9. Decision log

| Decision | Choice | Rationale |
|---|---|---|
| 主路線 | A: visibility flip + namespace + 補 args + 新 diff command | 三 hidden command 已有 impl；diff 邊界清楚；不動 graph internal |
| `mcp` 位置 | admin namespace | user 看 admin 找到；MCP entry 不該污染 agent surface |
| `shape_check` 位置 | top-level visible | LLM-facing drift detector，agent 在 API task 自動會用 |
| `shape_check` MCP tool 暴露 | 進 mcp tools 清單 | description 夠 specific（HTTP consumer↔Route drift），agent 不會誤呼叫；context cost 換 ROI 划算 |
| `verify_resolver` 位置 | admin namespace | Oracle JSONL 拿不到 → agent 用不了 → 不該污染 agent surface |
| `verify_resolver` self-baseline mode | 不做（由 `gnx diff --section bindings` 涵蓋） | 職責分離：oracle mode dev 用，self-baseline agent 用，二者命令分開 |
| 新 diff 命令名 | `gnx diff` | 跟 gnx 單字命令風格一致；跟 git diff 概念對齊但 context 內無歧義 |
| Section 結構 | `--section <s>` flag（非 subcommand） | 支援多選 + `all`；CLI help 列 possible values 清楚 |
| Section 命名 | `bindings` / `routes` / `contracts` / `all` | 跟既有 `gnx routes` / `gnx contracts` 對稱；`bindings`（複數）比 `resolver`（元件名）對 agent / user 直覺 |
| `--baseline` 預設 | **無預設，required** | 跨 commit 對比結果取決於 baseline 選擇，silent default 易誤導 |
| Baseline ref 接受形式 | branch / tag / SHA / HEAD~N / PR/<n> | Cover 常見 use case；PR/<n> 透過 gh CLI |
| Exit code | 0 if no error（即使有 changes） | diff 是 advisory，CI gate 由 `--strict` 未來加 |
| Output format | text(default) / json / toon | 對齊 gnx 其他 command 慣例 |
| Output verbosity | text 預設 top-10/section，`--verbose` 全列 | 平衡資訊密度 vs context cost |

---

## 10. Open questions

1. **`PR/<n>` 解析 fallback**：當 repo 不是 GitHub（GitLab / Gitea），是否提供 `--baseline-cmd <shell-cmd>` 讓 user 自訂解析腳本？Or 一律要求傳 SHA？
2. **Baseline graph cache 策略**：跑過一次 `--baseline origin/main` 的 graph dump，下次同 SHA 是否快取避免重跑？快取 invalidation 條件（gnx 版本變、analyzer 改動）？
3. **Edges section 未來引入時的 grouping 策略**：50-200 edges/PR 噪音多。grouping by `(source_file, target_file)` 還是 by enclosing module？需獨立評估再加。
4. **Symbols section 未來引入時的 modified 定義**：body 改算 modified？signature 改算 modified？rename 跨 file 算什麼？需 identity tracking 設計。
5. **`gnx diff` 對 monorepo 多 sub-graph**：當前 `gnx diff` 假設單一 graph；多 repo（`--repo @group`）需獨立評估。

---

## 11. Scope size

| 項目 | LOC |
|---|---|
| `admin mcp` move + `--format` | ~40 |
| `shape_check` un-hide + `--route` | ~50 |
| `gnx diff` shared baseline infra | ~150 |
| `gnx diff --section bindings` | ~150 |
| `gnx diff --section routes` | ~80 |
| `gnx diff --section contracts` | ~120 |
| `gnx diff` output formatters | ~80 |
| `~/.claude/skills/gnx/SKILL.md` updates | ~30 lines |
| Tests | ~200 |
| **Total** | **~870 LOC** |

中型 PR。一次 ship；CI 沿用既有 nextest workflow。
