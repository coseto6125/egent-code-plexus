# CompensatedBy RelType 升格 + find-transaction-patterns doc 修正

**Date**: 2026-05-25
**Follow-ups**: FU-2026-05-25-008 (CompensatedBy 升格, M-L), FU-2026-05-25-009 (過時 doc comment, S)
**Spec source of FU-008 deferral**: `docs/superpowers/specs/2026-05-25-impact-heuristic-callers-default-design.md`

## 目標

把 Saga 補償配對 (`compensate_/undo_/rollback_<verb_noun>` ↔ `<verb_noun>`) 從 `ecp find-transaction-patterns` 的即時 name-scan，升格成真正的 `RelType::CompensatedBy` 啟發式圖邊，於 index 時由新的 `post_process/saga_pairs.rs` pass 產生。

**一次解決三個問題**：
- **verb-sprawl**：standalone verb 的 Saga 半邊改為查圖邊，verb 收斂。
- **主路徑曝光**：邊進圖後 `ecp impact` BFS 免費 traverse、自動納入 `review`、可透過 MCP cypher 查（Cursor/Copilot 目前碰不到 standalone verb）。
- **hot-path 成本**：偵測一次性發生在 index time，查詢端零重算。

順帶修 FU-009：`find_tx_patterns.rs` doc comment 的「never enter the graph」+「deferred pending T5-33」已過時（T5-33 EventTopicMirror 已 land），retire Saga 半邊時一併改寫。

## Doc-comment 效益證明（CLAUDE.md next-action(1) gate）

新 `RelType::CompensatedBy` variant 的 enum doc comment：

> **(A) 圖完整性**：重構一個 Saga operation 時，`ecp impact <operation>` 的 BFS 能直接帶出其 compensator，不必 grep `undo_*`/`rollback_*` 命名猜測。
> **(C) 邊語意**：`Calls` 只表達「A 呼叫 B」，無法表達「B 是 A 的失敗補償」這個方向性語意——回滾分析需要這層獨立語意，`Calls` 不能替代。

這個效益可被具體寫出，故 variant 通過 gate、應加入。

## 架構與接點

新增 `crates/ecp-analyzer/src/post_process/saga_pairs.rs`，與 `event_topic_mirrors.rs` 同級。

| 接點 | 動作 |
|------|------|
| `crates/ecp-core/src/graph.rs` `RelType` enum (末尾, 現 `UsesPathLiteral` 之後) | append `CompensatedBy`（END，保 rkyv discriminant 穩定） |
| `graph.rs:99` `RelType::is_heuristic` | 加入 `\| Self::CompensatedBy` |
| `graph.rs:463` `ArchivedRelType::is_heuristic` | 同步加入 `\| Self::CompensatedBy` |
| `graph.rs` `RelType::as_str` | 加 `Self::CompensatedBy => "CompensatedBy"`（cypher `type(r)` 用） |
| `crates/ecp-analyzer/src/post_process/mod.rs` | `pub mod saga_pairs;` |
| `crates/ecp-analyzer/src/resolution/builder.rs` (~1688 decorates 之後) | 仿 `event_topic_mirrors::emit_edges` 註冊呼叫，回傳 count |
| `crates/ecp-cli/src/commands/find_tx_patterns.rs` | retire Saga 偵測 → 改查 `CompensatedBy` 邊；修 doc comment (FU-009) |
| `~/.claude/skills/ecp/_shared/cli/find-transaction-patterns.md:40` | 修 T5-33 過時敘述 (FU-009) |

## 資料流

`saga_pairs::emit_edges(local_graphs, symbol_table, string_pool, nodes, edges)` 在 builder 已組好 `nodes`（owner_class resolved）+ `edges`（`Calls` 已建）後執行：

1. 掃 `&[Node]`，依 `owner_class` 分組 Method/Function 節點。
2. 每組內建 `name → node_idx` map (O(1) lookup)。
3. 對每個 compensator（名稱 strip 出 `compensate/undo/rollback` 詞根後得 bare verb_noun），查同組是否有同名 operation。
4. 查 `edges` 確認 compensator 是否有 `Calls` edge 指向 operation（calls-back 證據）。
5. 發 `Edge { source: compensator_idx, target: operation_idx, rel_type: CompensatedBy, confidence, reason }`。

**邊方向**：`compensator → operation`，語意「此補償函式回滾該操作」（source 補償 target，比照 `OpensTxScope` 的 relation-not-direction 命名慣例）。

**confidence 分層**（沿用 `detect_saga_pairs` 現有公式）：
- `0.8` — compensator 有 `Calls`→operation 證據，`reason = saga:calls-back`
- `0.6` — 純命名配對無直接呼叫，`reason = saga:name-only`（真實 Saga 常透過 orchestrator 間接觸發，故仍收）

`is_heuristic()==true` → default `ecp impact` 隱藏、`requires_verification` 語意保留；reason 編碼證據等級讓 cypher 消費者自選嚴/寬，不必重 parse（同 `UsesPathLiteral` 在 reason 編 confidence 的做法）。

## 重用策略

`find_tx_patterns.rs` 既有的 `detect_saga_pairs` 操作 `ArchivedZeroCopyGraph`（已序列化圖），新 pass 操作 index-time 的 `&[Node]`/`&[Edge]` buffer——資料源不同，無法直接搬移，但**演算法同構**（owner_class 分組 + prefix strip + name 配對 + Calls 證據檢查）。

選定方案：
- 偵測核心邏輯（`COMPENSATOR_PREFIXES`、`strip_compensator_prefix`、name 配對、calls-back 檢查）抽到共享 helper，buffer 版與 archived 版各自薄包一層。
- `find_tx_patterns.rs` Saga 半邊 retire：`detect_saga_pairs` 刪除，`run()` 改從 graph 讀 `CompensatedBy` 邊組 JSON（輸出 schema 不變）。
- **Outbox 半邊不動**（FU-008 範圍只含 Saga）。

## 14 語言 name-pair 慣例（next-action(3)）

現有 `COMPENSATOR_PREFIXES = ["compensate_", "undo_", "rollback_"]` 只含 **snake_case**，對 camelCase（Java/Kotlin/C#/Swift/TS：`undoBooking`）、PascalCase（Go 匯出：`UndoBooking`）系統性漏抓。

**修正**：詞根改為 `compensate / undo / rollback` 三個語意 token，配對時涵蓋三種 case 邊界：
- snake_case：`undo_book_room` → 詞根 `undo` + suffix `book_room`，operation 名 `book_room`
- camelCase：`undoBookRoom` → 詞根 `undo` + suffix `bookRoom`，operation 名 `bookRoom`
- PascalCase：`UndoBookRoom` → 詞根 `Undo` + suffix `BookRoom`，operation 名 `BookRoom`

不引入語言別 hardcode：以 case-insensitive 詞根前綴匹配 + 後綴 case 還原推導 operation 名，跨 14 語言慣例一致（符合 generality 原則，corpus relayout 不破）。

## 測試

| 測試 | 驗證 |
|------|------|
| `saga_pairs_emit_compensatedby_calls_back` | compensator 有 Calls→op → 0.8 邊 + `saga:calls-back` |
| `saga_pairs_emit_compensatedby_name_only` | 純命名配對 → 0.6 邊 + `saga:name-only` |
| `saga_pairs_different_class_no_edge` | 跨 owner class 不配對 |
| `saga_pairs_camel_and_pascal_case` | `undoBookRoom`/`UndoBookRoom` 跨 case 慣例都配對（14 語言代理測試） |
| `compensatedby_is_heuristic_hidden_in_default_impact` | default `ecp impact` 隱藏、`--no-heuristic`/明確 cypher 才出 |
| `compensatedby_visibility_does_not_affect_risk_coverage` | 新邊不影響既有 risk/coverage（比照 commit `e74b8e6c`） |
| `find_tx_patterns_saga_reads_graph_edge` | retire 後 verb 從 `CompensatedBy` 邊讀，輸出 schema 不變 |

## Corpus 驗證（實作後）

單元測試過 ≠ pipeline 真的發邊（feedback: corpus-verify-not-parse-file）。實作後在 `.sample_repo` 跑 `ecp` 重 index + cypher：

```cypher
MATCH ()-[r:CompensatedBy]->() RETURN count(r), r.confidence
```

確認非零且 confidence 分層正確。同時跑既有 parity baseline 確認 `MirrorsField`/`EventTopicMirror` 數字不受影響。

## 範圍邊界（YAGNI）

- **只做 Saga**，Outbox 半邊與其 doc 不動。
- **不加動詞白名單**：confidence 已表達不確定性，白名單是 hardcode 且重複編碼。
- **不做 cross-class Saga**：保持同 owner class gate（與現有一致）。
