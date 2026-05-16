# Class Membership Post-Process Edges — `HasMethod` / `HasProperty`

**Date**: 2026-05-16
**Status**: Design (spec) — pre-implementation
**Goal**: 在 analyzer pipeline 末段加一支跨語言 post-process pass，從既有的 span 包含關係推導出 `HasMethod` / `HasProperty` edges，讓 LLM agent 對 Class 符號的 `gnx inspect` / `gnx impact` / `gnx cypher` query 不再回空集合。

**Related**:
- `crates/graph-nexus-core/src/graph.rs` — `RelType::HasMethod` / `HasProperty` 已宣告但無人 emit
- `crates/graph-nexus-core/src/analyzer/pipeline.rs` — parser 跑完後的整合點
- `crates/graph-nexus-cli/src/commands/inspect.rs` — Class 視角的輸出
- 後續 PR 2 / PR 3：type-annotation `References` + cross-lang fixture polish

---

## 1. Problem statement

### 1.1 Graph 中 Class 是孤島

實測（5 月 16 日 session 內驗證，HEAD 為 `ee3dd4f`）：

```
analyzer 實際 emit 的 RelType 統計（grep "RelType::" crates/graph-nexus-analyzer/src/）：
  Calls(4 emit 處) / References(6) / Fetches(5) / HandlesRoute(2)
  StepInProcess(1) / Extends(1) / Accesses(1)

從未被 emit 但 enum 宣告了的：
  HasMethod / HasProperty / Defines / Implements / Imports  ← 5 個全空

Cypher 驗證：
  MATCH (a:Class)-[r:HasMethod]->(b:Method) RETURN a,b
  → 297 個 Class 全 0 rows
  MATCH (a:Class)-[r:HasProperty]->(b:Property) RETURN a,b
  → 0 rows
```

### 1.2 對 LLM agent 的影響

`gnx inspect Resolver`（Class）目前回：

```
status: found
symbol: { kind: Class, startLine: 46, endLine: 58, ... }
incoming: {}
outgoing: {}
impact_upstream_1hop: []
blind_spots: []
processes: []
```

對 agent 任何「這個 Class 有哪些 method / property」「誰把它當 type 用」的問題，inspect 是死路。Agent 被迫 fallback 到 `Read` 整檔或 `grep <ClassName>` — token expensive、低結構性、不可靠（doc / string mention noise）。

### 1.3 Root cause

每個 language parser 各自 emit edges 時 mental model 是「local AST 看到什麼節點 → 建什麼節點」。Parser 不會做跨節點推導：

- Parser 看到 `class Foo: def bar(self)` 時，建出 Class node + Method node，**但 parser 不主動建立兩者之間的 edge**
- 也沒人在 parser 跑完後做「掃所有 Class，找其 span 內的 Method，建邊」這件事

結果：所有 31 種語言對 Class membership 統一缺資料，**不是 parser 漏寫，是 pipeline 漏一個 step**。

---

## 2. Scope

### 2.1 In scope（本 PR）

| 動作 | 對象 |
|---|---|
| 新增 cross-language post-process module | `crates/graph-nexus-core/src/analyzer/post_process/class_membership.rs` |
| 整合進 pipeline 末段 | `crates/graph-nexus-core/src/analyzer/pipeline.rs` |
| Emit `RelType::HasMethod` edges | Class span ⊃ Method span，同檔，innermost enclosing |
| Emit `RelType::HasProperty` edges | Class span ⊃ Property span，同檔，innermost enclosing |
| 擴充 `gnx inspect` 對 Class kind 的輸出 | 加 `contained_methods` / `contained_properties` 欄位 |
| Cross-language fixture tests | Rust struct / TypeScript class / Python class / Ruby class（4 種代表） |

### 2.2 Out of scope（明示延後）

- **PR 2** — type-annotation `References` edges（從 param/return type string 對應 Class node）。風險集中在跨語言 type string normalize（`&Resolver` / `Option<Resolver>` / `Arc<Mutex<Resolver>>` 都要對到同個 Class node），需獨立 spec。
- **PR 3** — inspect Class 完整輸出格式 polish + 補齊其他語言 fixtures（C++ / Swift / Java / Go interface / Kotlin / C# / PHP / Dart）
- `RelType::Defines` / `Implements` / `Imports` — 不在 Class membership 核心問題範圍。`Implements` 部分可能跟既有 `Extends` overlap，獨立評估
- **不動 parser** — 所有改動在 core analyzer，per-language 改動為 0
- **不改 graph schema** — `RelType` enum 已宣告 `HasMethod` / `HasProperty`，只是補實作

### 2.3 為什麼不一次梭三 PR

PR 2 的 type-annotation References 一旦寫歪會造成「半正確 edges」 — agent 信了但實際 miss 一半 callers，比「無 edge」更糟（沒 edge 時 agent fallback grep，至少 noisy 但 complete）。先用 PR 1 拿觀察數據（agent 是否真的開始用 `inspect Class`，token cost 變化）再決定 PR 2 要不要做、做多細，是治理風險的合理切法。

---

## 3. Approach

採用 **Approach A：純 post-process pass，讀既有 span / file_idx，emit 新 edges**。

理由：
- Span / file_idx 是**普世存在**的 node 屬性（所有 parser 都填，因為 tree-sitter 給 byte range，pipeline 一律 normalize）
- Class membership 是**結構性事實**而非 language-specific 邏輯 — 「method 在 class body 內」這定義對所有 OOP 語言一致
- 1 支 module 對 31 語言生效，不需 per-parser maintenance
- 風險低 — 只新增 edges，不改既有 node / edge / serialization layout

替代方案（已 rejected）：

- **B. 動 31 個 parser 各自補 emit** — 工程量爆炸 + per-language quirks 多（e.g. Ruby `def_delegator` macro、Python `__slots__`、Rust `impl Foo for Bar { fn ... }` 拆兩個 node group）。Tier-1 parser 工作剛完就再來一輪不划算
- **C. 不 emit edges 改在 inspect 查詢時即時計算** — Cypher / impact 等其他 query path 拿不到，違反「graph 是 single source of truth」原則；inspect 每次都掃全 graph filter file_idx → O(N) per query

---

## 4. Algorithm

### 4.1 Core span containment

對每個 file：
1. 收集該 file 內所有 Class node 與 Method / Property node（用 file_idx 篩）
2. 對每個 Method / Property，掃該檔所有 Class，挑「span 最小且包含其 span 的 Class」（innermost enclosing），emit `HasMethod` / `HasProperty` edge

### 4.2 "Innermost enclosing" 規則

```
class Outer:                 # span (1, _, 10, _)
    def outer_method(self):  # span (2, _, 3, _) → HasMethod(Outer)
        ...
    class Inner:             # span (5, _, 9, _)
        def inner_method(self):  # span (6, _, 8, _) → HasMethod(Inner)，不是 Outer
            ...
```

Method 屬於**最內層**包含其 span 的 Class。避免「nested class 的 method 被誤判為屬於外層 class」 → 違背 OOP 語意。

### 4.3 Span containment 判定

```rust
fn is_contained(child: Span, parent: Span) -> bool {
    // parent.start ≤ child.start AND parent.end ≥ child.end
    (parent.0 < child.0 || (parent.0 == child.0 && parent.1 <= child.1))
        && (parent.2 > child.2 || (parent.2 == child.2 && parent.3 >= child.3))
}
```

採 inclusive 邊界（start ≤ start、end ≥ end），lexicographic 比較 (line, col)。Caller 端用 node identity 排除自比（class 不會被當作自己的 member），不靠 span 嚴格不等做識別。

### 4.4 Edge case：top-level functions

Top-level（module-level）的 `def foo():` 在 Python / TS 等是 `NodeKind::Function`，不是 `Method`，本 pass 不處理。確保 `Function` kind 不會被誤連到 surrounding scope。

### 4.5 Complexity

`O(F * (N_class * N_member))` where F = file count, N_class / N_member = per-file。實務上每檔 N_class * N_member 數量級 < 100，整 repo < 10ms scan（資料量參考：當前 gnx-rs 6006 symbols / 12311 rels 全建 ~1.5s）。

---

## 5. Pipeline integration

### 5.1 Insertion point

`crates/graph-nexus-core/src/analyzer/pipeline.rs` 的主流程末段：

```
parse_file_raw(...) for each file
  ↓
build_string_pool + node interning
  ↓
edge resolution (resolver 各 tier)
  ↓
[NEW] post_process::class_membership::emit_edges(&mut graph)
  ↓
serialize graph.bin
```

理由：演算法本身 per-file（class 跟其 method 必同檔），但 mutation 必須在所有 node 都 inserted、resolver 跑完、serialize 前 — 確保 Node array 已 finalize，邊新增不破壞既有 index。

### 5.2 接口

```rust
// crates/graph-nexus-core/src/analyzer/post_process/mod.rs
pub mod class_membership;

// crates/graph-nexus-core/src/analyzer/post_process/class_membership.rs
pub fn emit_edges(graph: &mut Graph) -> usize { /* returns edges emitted */ }
```

回傳 emit 數量便於 pipeline log / 測試斷言。

---

## 6. inspect output

### 6.1 Class 視角擴充

`gnx inspect <name>` 當 symbol kind = Class 時，現有輸出加 2 個欄位：

```jsonc
{
  "status": "found",
  "symbol": { "kind": "Class", "name": "Resolver", ... },
  "incoming": { /* References / Extends 等，已存在 */ },
  "outgoing": { /* HasMethod / HasProperty 兩 buckets 自動填入 */ },
  // NEW: derived view，比 outgoing.HasMethod 更 agent-friendly
  "contained_methods": [
    { "name": "resolve_symbol", "filePath": "...", "line": 47 },
    { "name": "resolve_call_target", "filePath": "...", "line": 52 }
  ],
  "contained_properties": [
    { "name": "registry", "filePath": "...", "line": 48 }
  ],
  "impact_upstream_1hop": [/* 不變 */]
}
```

注意：HasMethod 同時填進 `outgoing.HasMethod`（generic edge view）跟 `contained_methods`（Class-specific friendly view）— 後者是前者的 shaped projection，agent 不用自己 parse outgoing key。

### 6.2 非 Class kind 的 inspect

對 Function / Method / Property 等，輸出**不變**。本 PR 只擴充 Class 視角。

### 6.3 `outgoing.HasMethod` 對其他工具自動生效

- `gnx cypher "MATCH (a:Class)-[:HasMethod]->(b:Method) RETURN a,b"` 終於有 rows
- `gnx impact <Class>` upstream BFS 經 HasMethod edge 觸及 method，blast radius 變寬（method 的 callers 也納入）

---

## 7. Tests

### 7.1 Unit tests（post-process module）

`crates/graph-nexus-core/src/analyzer/post_process/class_membership.rs`：

| Test | 內容 |
|---|---|
| `single_class_emits_methods` | 1 class with 3 methods → 3 HasMethod edges |
| `nested_class_attributes_methods_to_innermost` | Outer 內含 Inner，Inner 內含 method → method 屬 Inner |
| `top_level_function_not_attributed` | Class 外的 module-level fn 不被 emit |
| `class_with_only_properties` | 純資料 class → 只有 HasProperty edges |
| `empty_class_no_edges` | Class 內無 member → 0 edges |
| `multi_class_same_file_disjoint_spans` | 同檔多 class，各自 method 不混 |

### 7.2 Integration tests（cross-language fixtures）

`crates/graph-nexus-cli/tests/class_membership_inspect.rs`：

| 語言 | Fixture | 斷言 |
|---|---|---|
| Rust | `struct Foo { a: i32 } impl Foo { fn bar(&self) {} }` | `inspect Foo` 回 `contained_methods: [bar]`, `contained_properties: [a]` — 驗 §8 R1（impl block 內 method 需歸給對應 struct Class）。若 parser 把 `impl Foo` 建成獨立 node 而非 method 直接在 struct span 內，此 test 會抓到 |
| TypeScript | `class Foo { x = 1; bar() {} }` | 同上對應 |
| Python | `class Foo: x = 1; def bar(self): pass` | 同上 |
| Ruby | `class Foo; attr_reader :x; def bar; end; end` | 同上 |

每個 fixture 走 `gnx admin index` 跑完後 `gnx inspect Foo --format json` 斷言輸出 shape。

### 7.3 Cypher 行為驗證

`crates/graph-nexus-cli/tests/cypher_has_method.rs` 加 1 test：

- Build fixture graph w/ 2 classes
- Query: `MATCH (a:Class)-[r:HasMethod]->(b:Method) RETURN a,b`
- Assert: rows count == expected method count

---

## 8. Risks

| Risk | 機率 | Mitigation |
|---|---|---|
| Rust `impl X for Y { fn ... }` 中 method 屬於 impl block 不是 struct itself，parser 拆兩種 node | 中 | 統一 rule：`HasMethod` 連到 `kind=Class`（包含 struct）的最內包含 node；impl block 若 parser 建成獨立 node 需 skip。tests/rust-impl fixture 驗證 |
| Python decorator / metaclass 改變 method 歸屬 | 低 | 純 span 推導不受 decorator 影響（decorator 不改 source 行位置）。已知不解：dynamic `setattr` 加的 method 本就不在 parser 視野內 |
| Spec 跑出未預期的 edge 爆炸（每 class 數十 methods × N classes） | 低 | 同 §4.5 — O(N_class × N_member) per file，常數小；先測 worst-case fixture（PyTorch-like 大 class）驗證 |
| `inspect` 輸出 schema 變動破壞既有下游 parser | 低 | 新欄位 `contained_methods` / `contained_properties` 是純 additive；不改既有欄位 |

---

## 9. Open questions

1. **Constructor 算 method？** Rust 沒 ctor 概念，TS / Python 有 `constructor` / `__init__`。建議：**算**，視作普通 method emit `HasMethod`。LLM agent 想要的就是「Class 有哪些可呼叫的東西」。
2. **Interface 是否也適用？** `NodeKind::Interface` 在 TS / Java 是另一個 kind。本 PR 先**不**處理 — interface members 通常是 method signature 而非 impl，後續可加 `Interface -[HasMethod]-> Method` 但需另定 spec
3. **`gnx inspect Method` 反向查 parent class？** Cypher 已能 `MATCH (a:Class)-[:HasMethod]->(b:Method) WHERE b.name='foo' RETURN a`。inspect 視角是否要直接 surface `member_of`？延後 PR 3 評估

---

## 10. Acceptance criteria

- [ ] `crates/graph-nexus-core/src/analyzer/post_process/class_membership.rs` 通過 6 unit tests
- [ ] `crates/graph-nexus-cli/tests/class_membership_inspect.rs` 通過 4 跨語言 fixture tests
- [ ] `crates/graph-nexus-cli/tests/cypher_has_method.rs` 通過 1 Cypher 行為 test
- [ ] `gnx inspect <Class>` 對至少 4 種語言的 Class 回非空 `contained_methods` / `contained_properties`
- [ ] `gnx cypher "MATCH (a:Class)-[:HasMethod]->(b:Method) RETURN a,b"` 在 gnx-rs 自身 repo 跑出 > 100 rows（驗證 graph 已實際填充）
- [ ] 整體 graph build time 增加 < 5%（資料量參考：當前 gnx-rs 6006 symbols / 12311 rels 全建 ~1.5s）
