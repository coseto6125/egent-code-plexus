# Class Membership Post-Process Edges — `HasMethod` / `HasProperty`

**Date**: 2026-05-16
**Status**: Design (spec v2) — pre-implementation, validation-informed
**Goal**: 在 analyzer pipeline 末段加一支跨語言 post-process pass，從既有的 span 包含關係（+ Rust `build_impl_map`）推導出 `HasMethod` / `HasProperty` edges，讓 LLM agent 對 Class 符號的 `cgn inspect` / `cgn impact` / `cgn cypher` query 不再回空集合。

**Status note (v2)**: Spec v1 已 squash-merge 進 main（PR #33, commit `e074737`）。本 v2 整合 5 項實測 validation 跟 5-agent design 評估後的修正。

**Related**:
- `crates/cgn-core/src/graph.rs` — `RelType::HasMethod` / `HasProperty` 已宣告但無人 emit（cgn-rs 自身驗證 297 個 Class 全 0 HasMethod edges）
- `crates/cgn-analyzer/src/framework_helpers.rs` — **`enclosing_class` / `enumerate_class_methods` 已存在**，目前供 framework detection 使用
- `crates/cgn-analyzer/src/rust/receiver_types.rs` — **`build_impl_map` 已存在**，目前供 Rust receiver-type call 解析使用
- `crates/cgn-analyzer/src/resolution/builder.rs` — 實際 graph 構造點（v1 spec 寫錯為 `pipeline.rs`）
- 上游 `gitnexus` 的 `reconcileOwnership` pipeline pass（同設計，已 production 驗證）
- 後續 PR 2 / PR 3：type-annotation `References` + cross-lang fixture polish

---

## 1. Problem statement

### 1.1 Graph 中 Class 是孤島

實測（5 月 16 日 session 內驗證，HEAD `587534d`）：

```
analyzer 實際 emit 的 RelType 統計：
  Calls(4 emit 處) / References(6) / Fetches(5) / HandlesRoute(2)
  StepInProcess(1) / Extends(1) / Accesses(1)

從未被 emit 但 enum 宣告了的：
  HasMethod / HasProperty / Defines / Implements / Imports  ← 5 個全空

Cypher 驗證：
  MATCH (a:Class)-[r:HasMethod]->(b:Method) RETURN a,b
  → 297 個 Class 全 0 rows
```

### 1.2 對 LLM agent 的影響

`cgn inspect Resolver`（Class）目前回 `incoming={} outgoing={} impact_upstream_1hop=[]` — agent 任何「class 有什麼 method」「誰把它當 type」的問題，inspect 死路 → fallback 到 `Read` 整檔或 `grep <ClassName>`，token expensive、低結構性、不可靠。

### 1.3 Root cause（跨語言 parser 模式）

每個 language parser 各自 emit edges 時的 mental model 是「local AST 看到什麼節點 → 建什麼節點」。**Parser 不會主動跨節點建立 membership edge**，也沒有後處理 pass 補。所有 31 種語言對 Class membership 統一缺資料 — 不是 parser 漏寫，是 pipeline 漏一個 step。

### 1.4 Validation findings（v2 新增）

Spec v1 起草時的 5 個假設，實測結果：

| V# | 假設 | 結果 | 對 spec 影響 |
|---|---|---|---|
| V1 | Rust struct + impl method span 在 Class span 內 | ❌ FAIL | inherent impl `fn` 被標 **Function**、span 在 Class span 外。**Rust 不適用純 span containment** — 改用 `build_impl_map` 補強 |
| V2 | Python class method kind = Method | ❌ FAIL | Python 0 個 Method node（17 Class + 119 Function）— 所有 `def` 都是 Function（**正確**，反映 Python 語意：方法就是 function + descriptor） |
| V3 | Property kind 有 emit | ✅ PASS | 331 rows，主要 vendor C struct fields，但 fixture `.go`/`.c` 也有 |
| V4 | TS / Ruby class span 包含 method span | ✅ PASS | TS UsersController (3-25) 包含 findOne (7-9)；Ruby fixture 類似 |
| V5 | Graph 在 post-process 可 mutate | ⚠️ PARTIAL | `edges: Vec<Edge>` mutable，但 **CSR `out_offsets` / `in_offsets` 必須在 push 後重建**。Insertion point 改為 `analyzer/resolution/builder.rs::build()` line 907-919（v1 spec 寫的 `pipeline.rs` 是錯的） |

### 1.5 已有的工具箱（v2 新增 — 不從零寫）

`analyzer/src/framework_helpers.rs` 既有 helpers（目前供 framework detection 使用）：

```rust
// 跨語言找 innermost enclosing Class by smallest span area
pub fn enclosing_class(nodes: &[RawNode], inner_span: Span) -> Option<(String, Span)>;

// 列 class body 內 Function/Method（注意：接受 Function|Method 兩種 kind —
// 這正好覆蓋 Python def-as-Function case）
pub fn enumerate_class_methods(nodes: &[RawNode], class_span: Span, exclude_name: &str) -> Vec<String>;
```

`analyzer/src/rust/receiver_types.rs::build_impl_map` — 走 Rust `impl_item` AST 建 `fn_name → impl_type_name`（inherent + trait impl 都涵蓋），bridge Rust 的 struct/impl 拆分。

**這些 helper 已實作完整、有 unit test，目前服務於其他 caller**。Post-process module 直接 reuse / adapt，不重寫。

---

## 2. Scope

### 2.1 In scope（本 PR）

| 動作 | 對象 |
|---|---|
| 新增 cross-language post-process module | `crates/cgn-analyzer/src/post_process/class_membership.rs` |
| 整合進 builder 末段（CSR 計算前） | `crates/cgn-analyzer/src/resolution/builder.rs::build()` line 907 附近 |
| Emit `RelType::HasMethod` edges | Class span ⊃ Method/**Function** span，innermost enclosing |
| Emit `RelType::HasMethod` for Rust impl-mapped fns | 用 `build_impl_map` 對 inherent / trait impl 的 fn 補 edge |
| Emit `RelType::HasProperty` edges | Class span ⊃ Property span |
| 擴充 `cgn inspect` 對 Class kind 的輸出 | 加 `contained_methods` / `contained_properties` 欄位 |
| 更新 SKILL.md 加 1-line cypher 慣例提示 | `docs/skills/cgn.md` + `~/.claude/skills/cgn/SKILL.md` |
| Cross-language fixture tests | TS / Ruby / Python / Rust trait impl / Rust inherent impl（5 種代表） |

### 2.2 Out of scope（明示延後）

- **PR 2** — type-annotation `References` edges（從 param/return type string 對應 Class node）。風險集中在跨語言 type string normalize（`&Resolver` / `Option<Resolver>` / `Arc<Mutex<Resolver>>`），需獨立 spec
- **PR 3** — inspect Class 完整輸出格式 polish + 補齊其他語言 fixtures（C++ / Swift / Java / Go / Kotlin / C# / PHP / Dart）
- `RelType::Defines` / `Implements` / `Imports` — 不在 PR 1 範圍。**特別注意**：5-agent design review（見 §11）決議 **不**用 `Defines` 區分 Python def — 採 B.1 策略，HasMethod 統一 emit，避免 cypher dispatching 負擔
- **不動 parser** — 所有改動在 analyzer post-process，per-language 改動為 0
- **不改 graph schema** — `RelType` enum 已宣告 `HasMethod` / `HasProperty`，只是補實作
- **不改 node kind field** — Python `def` 仍 kind=Function、Rust associated fn 仍 kind=Function（preserve language semantics）

### 2.3 Emission strategy: B.1（v2 確定）

5-agent independent review（Haiku × 5，無 prior context）一致選 B.1：

- **單一 edge type** `HasMethod`：Class → 任何 callable member（target kind 可為 Method 或 Function）
- **不分流到 `Defines`**：避免 LLM agent 學 conditional dispatch（`[:HasMethod]` vs `[:HasMethod|Defines]`）
- **Cypher 慣例**：`MATCH (c:Class)-[:HasMethod]->(m) RETURN m` 全語言 work，**不在 query 加 `:Method` filter**
- **SKILL 一行 note**：「`HasMethod` target kind 由 parser 決定（Method 或 Function），cypher query 不限定 target kind」

理由（合議結論）：
- LLM agents 對 single predictable pattern reliability > 對 multi-edge-type dispatching
- B.4（HasMethod + Defines 分流）的「語意精確」不 compound 到更好 cypher，反而加 branching 負擔
- `cgn inspect Class` 是主要 UI 表面、`contained_methods` 已將異質性 hide 掉，cypher 不必重複表達

---

## 3. Approach

採用 **Approach A: 純 post-process pass，讀既有 span / file_idx + Rust impl_map，emit 新 edges**。

理由：
- Span / file_idx 是普世存在的 node 屬性
- Class membership 是結構性事實而非 language-specific 邏輯
- **已有 helpers 可直接 reuse**（`enclosing_class` / `enumerate_class_methods` / `build_impl_map`）— 不重複造輪子
- 跨 31 語言生效，不需 per-parser maintenance
- 風險低 — 只新增 edges，不改既有 node / edge / serialization

替代方案（已 rejected）：

- **B. 動 31 個 parser 各自補 emit** — 工程量爆炸 + per-language quirks 多
- **C. 不 emit edges 改在 inspect 查詢時即時計算** — Cypher / impact 等其他 query path 拿不到，違反「graph 是 single source of truth」原則

### 3.1 上游 gitnexus 的 precedent（v2 新增）

上游 gitnexus（`._source_code/ARCHITECTURE.md` L258）的 `reconcileOwnership` pipeline pass 完全是同設計：

> "a shim for languages whose legacy extractor doesn't resolve `enclosingClassId` at parse time (**Python class-body methods are the canonical case**). It walks `parsed.localDefs[i].ownerId` after `populateOwners` and registers any missed methods/fields into the model. Idempotent — safe to re-run."

上游已 production 驗證此設計可行；cgn-rs 補 post-process pass 等同把上游決策補齊。

---

## 4. Algorithm

### 4.1 Two-pass strategy（跨語言 span 推導 + Rust impl bridge）

```
Pass 1: 跨語言 span containment（適用 TS/Ruby/Java/PHP/Dart/Swift/C++/C#/Python 等
        — 凡 class body 語法上包含 method 的）

  for each Class node c in graph:
      members = nodes in same file whose span is contained by c.span
                AND kind in {Method, Function, Property}
                AND innermost enclosing class is c

      for each m in members:
          if m.kind in {Method, Function}:
              emit edge: c -[HasMethod]-> m       # B.1: target kind 不限
          elif m.kind == Property:
              emit edge: c -[HasProperty]-> m

Pass 2: Rust impl bridge（適用 Rust inherent + trait impl — class span
        不含 impl block body 的特殊 case）

  for each Rust source file f:
      impl_map = build_impl_map(f.ast, f.source)  # 既存 helper

      for each entry (fn_name, impl_type_name) in impl_map.entries:
          class_node = lookup Class node by name=impl_type_name in file f
          fn_node    = lookup Method|Function node by name=fn_name in file f
          if class_node and fn_node:
              emit edge: class_node -[HasMethod]-> fn_node
```

Pass 1 涵蓋 30 個語言（含 Python — `enumerate_class_methods` 已接受 Function kind）。
Pass 2 涵蓋 Rust 的 struct ↔ impl 拆分特例。

### 4.2 "Innermost enclosing" 規則（避免 nested class）

```python
class Outer:                 # span (1, _, 10, _)
    def outer_method(self):  # span (2, _, 3, _) → HasMethod(Outer)
        ...
    class Inner:             # span (5, _, 9, _)
        def inner_method(self):  # span (6, _, 8, _) → HasMethod(Inner)，不是 Outer
            ...
```

Method 屬於**最內層**包含其 span 的 Class。`enumerate_class_methods` helper 已用 `min_by_key(span_area)` 實作此邏輯（line 75）— 直接 reuse。

### 4.3 Span containment 判定

`framework_helpers::span_contains` 已實作 inclusive 邊界（start ≤ start && end ≥ end）。Caller 端用 node identity 排除自比。

### 4.4 Edge case：top-level functions

Top-level `def foo():` / `function foo()` — 不被任何 Class 包含 → `enumerate_class_methods` 自然不收 → 不 emit edge。**正確行為**。

### 4.5 Complexity

`O(F × N_class × N_member)` per file。實測 cgn-rs 6118 symbols / 12532 rels 全建 ~1.5s；post-process 估 < 100ms（Pass 1 線性掃 + Pass 2 Rust files only）。

---

## 5. Pipeline integration

### 5.1 Insertion point（v2 修正）

**真正的 insertion point** 在 `crates/cgn-analyzer/src/resolution/builder.rs::build()` line 907-919 之間：

```rust
pub fn build(self) -> ZeroCopyGraph {
    // ... resolver tier processing ...
    edges.push(...);  // line 907 — last resolver edge push

    // [NEW] post_process::class_membership::emit_edges(&mut edges, &nodes, &files);

    // line 919 — CSR offset computation begins
    let mut out_offsets = vec![0; num_nodes + 1];
    // ...
}
```

**關鍵**：必須在 line 919（CSR `out_offsets` 計算開始）之前 push edges。push 之後 CSR 重建會自動把新 edges 索引進去。

### 5.2 接口

```rust
// crates/cgn-analyzer/src/post_process/mod.rs
pub mod class_membership;

// crates/cgn-analyzer/src/post_process/class_membership.rs
pub fn emit_edges(
    nodes: &[Node],
    edges: &mut Vec<Edge>,
    files: &[File],
    rust_impl_maps: &HashMap<u32, ImplMap>,  // 預計算 per Rust file
) -> usize;  // returns # edges emitted, for log/test
```

Rust `impl_maps` 由 caller（`build()`）預先 collect — 因為 `build_impl_map` 需要 tree-sitter `Node<'_>`（lifetime-bound），在 builder 整合 stage 才能跑。

---

## 6. inspect output

### 6.1 Class 視角擴充

`cgn inspect <name>` 當 symbol kind = Class 時，現有輸出加 2 個欄位：

```jsonc
{
  "status": "found",
  "symbol": { "kind": "Class", "name": "Resolver", ... },
  "incoming": { /* References / Extends 等，已存在 */ },
  "outgoing": { /* HasMethod / HasProperty buckets 自動填入 */ },
  // NEW: derived view，跨 target kind unified
  "contained_methods": [
    { "name": "resolve_symbol", "kind": "Method",   "filePath": "...", "line": 47 },
    { "name": "new",            "kind": "Function", "filePath": "...", "line": 60 }  // Rust assoc fn
  ],
  "contained_properties": [
    { "name": "registry", "filePath": "...", "line": 48 }
  ],
  "impact_upstream_1hop": [/* 不變 */]
}
```

**B.1 注意**：`contained_methods` 列表中**保留每個 entry 的 `kind` field**，agent 可選擇性區分 Method（true method with receiver）vs Function（associated/static-like）。預設不過濾。

### 6.2 非 Class kind 的 inspect

對 Function / Method / Property 等，輸出**不變**。本 PR 只擴充 Class 視角。

### 6.3 其他工具自動受益

- `cgn cypher "MATCH (a:Class)-[:HasMethod]->(b) RETURN a,b"` 終於有 rows
- `cgn impact <Class>` upstream BFS 經 HasMethod edge 觸及 method，blast radius 變寬

---

## 7. Tests

### 7.1 Unit tests（post-process module）

`crates/cgn-analyzer/src/post_process/class_membership.rs`：

| Test | 內容 |
|---|---|
| `single_class_emits_methods` | 1 class with 3 methods → 3 HasMethod edges |
| `nested_class_attributes_methods_to_innermost` | Outer 內含 Inner，Inner 內含 method → method 屬 Inner |
| `top_level_function_not_attributed` | Class 外的 module-level fn 不被 emit |
| `class_with_only_properties` | 純資料 class → 只有 HasProperty edges |
| `empty_class_no_edges` | Class 內無 member → 0 edges |
| `multi_class_same_file_disjoint_spans` | 同檔多 class，各自 method 不混 |
| `rust_inherent_impl_via_impl_map` | `impl Foo { fn bar() {} }` → `Foo -[HasMethod]-> bar`（target kind=Function） |
| `rust_trait_impl_via_impl_map` | `impl Trait for Foo { fn bar(&self) {} }` → `Foo -[HasMethod]-> bar`（target kind=Method） |
| `python_def_as_function_still_emits` | `class Foo: def bar(self):` → `Foo -[HasMethod]-> bar`（target kind=Function） |

### 7.2 Integration tests（cross-language fixtures）

`crates/cgn-cli/tests/class_membership_inspect.rs`：

| 語言 | Fixture | 斷言重點 |
|---|---|---|
| TypeScript | `class Foo { x = 1; bar() {} }` | `contained_methods: [{name:"bar", kind:"Method"}]`, `contained_properties: [{name:"x"}]` |
| Ruby | `class Foo; attr_reader :x; def bar; end; end` | 對應 Method + Property |
| Python | `class Foo: x = 1\n    def bar(self): pass` | `contained_methods: [{name:"bar", kind:"Function"}]` — **kind 保留 Function**，驗 B.1 |
| Rust trait impl | `struct Foo; impl Display for Foo { fn fmt(&self,_) {} }` | `contained_methods: [{name:"fmt", kind:"Method"}]` — 驗 Pass 2 trait impl bridge |
| Rust inherent impl | `struct Foo; impl Foo { fn new() -> Self {} }` | `contained_methods: [{name:"new", kind:"Function"}]` — 驗 Pass 2 inherent impl bridge |

### 7.3 Cypher 行為驗證

`crates/cgn-cli/tests/cypher_has_method.rs`：

```rust
// 驗 B.1 慣例：query 不加 target kind filter 全語言 work
let q = "MATCH (a:Class)-[r:HasMethod]->(b) RETURN a,b";
assert!(rows.count >= expected_total);  // 涵蓋 Python def + Rust assoc fn + 其他

// 反例：加 target kind filter 會漏 Python/Rust assoc
let q2 = "MATCH (a:Class)-[r:HasMethod]->(b:Method) RETURN a,b";
let rows2 = run(q2);
// rows2.count < rows.count — 這是預期行為，文件化於 SKILL
```

---

## 8. Risks

| Risk | 機率 | Mitigation |
|---|---|---|
| Rust `impl X for Y` method 屬於 impl block 不是 struct itself | 中 | **`build_impl_map` 既存 helper 解決**（fn_name → impl_type_name）— Pass 2 直接用 |
| Python decorator / metaclass 改變 method 歸屬 | 低 | 純 span 推導不受 decorator 影響；dynamic `setattr` 加的 method 本就不在 parser 視野內 |
| 邊爆炸（每 class 數十 methods × N classes） | 低 | O(N_class × N_member) per file，常數小；先測 worst-case fixture |
| `inspect` 輸出 schema 變動破壞既有下游 parser | 低 | 新欄位 `contained_methods` / `contained_properties` 純 additive |
| CSR `out_offsets` / `in_offsets` 重建漏 edge | 低 | Insertion point 在 line 907 → line 919 之間，CSR 計算自動 reflect 新 edges。配 `rust_inherent_impl_via_impl_map` test 端到端驗證 |
| LLM agent 加 `:Method` filter 漏 Python/Rust assoc fn | 中 | SKILL.md 加 1 行 cypher 慣例提示；`inspect` 是主要 surface 已 hide 異質性 |
| Rust 同名 fn 在多個 impl block（`impl Dog` + `impl Cat` 都有 `new`） | 低 | `build_impl_map.entries` 已是 HashMap by fn_name；同名衝突取最後（記錄 in `ImplMap` 註解 line 26-28）。Post-process emit 對所有匹配 emit edge，cypher 拿到正確 set |

---

## 9. Open questions

1. **Constructor 算 method？** 跨語言 ctor 處理：TS `constructor` kind=Method、Python `__init__` kind=Function、Rust `new` 是 associated fn kind=Function。**建議：算**（B.1 全收），統一邏輯，agent 看 `kind` 自己選擇是否區分
2. **Interface 是否也適用？** `NodeKind::Interface` 在 TS / Java 是另一個 kind。本 PR 先**不**處理 — interface members 通常是 method signature 而非 impl，後續可加 `Interface -[HasMethod]-> Method` 但需另定 spec
3. **`cgn inspect Method` 反向查 parent class？** Cypher 已能 `MATCH (a:Class)-[:HasMethod]->(b) WHERE b.name='foo' RETURN a`。inspect 視角是否要直接 surface `member_of`？延後 PR 3 評估

---

## 10. Acceptance criteria

- [ ] `crates/cgn-analyzer/src/post_process/class_membership.rs` 通過 9 unit tests
- [ ] `crates/cgn-cli/tests/class_membership_inspect.rs` 通過 5 跨語言 fixture tests
- [ ] `crates/cgn-cli/tests/cypher_has_method.rs` 通過 B.1 慣例驗證 test
- [ ] `cgn inspect <Class>` 對至少 5 種語言（TS / Ruby / Python / Rust 兩種 impl）回非空 `contained_methods`
- [ ] `cgn cypher "MATCH (a:Class)-[:HasMethod]->(b) RETURN a,b"` 在 cgn-rs 自身 repo 跑出 > 200 rows
- [ ] 整體 graph build time 增加 < 5%（基線 ~1.5s for 6118 symbols / 12532 rels）
- [ ] SKILL.md 加入 1 行 cypher 慣例提示（不加 target kind filter）

---

## 11. Design history（v2 新增）

### 11.1 Emission strategy 選擇歷程

設計過程考慮過 4 種策略，最終由 5-agent independent review 決議：

| Strategy | 描述 | 為何 rejected |
|---|---|---|
| B.2 | Promote Python def → Method, Rust assoc fn → Method | 改 node kind field — 影響 resolver / impact filter / 其他 cypher query 的 blast radius 大；**且誤表 Python / Rust 語意**（Python def 本來就是 function；Rust assoc fn 跟 method 是不同概念） |
| B.4 | 兩 edge type：HasMethod (kind=Method 嚴格) + Defines (kind=Function) | 語意精確但 LLM agent 需學 conditional dispatch (`[:HasMethod]` vs `[:HasMethod\|Defines]`)；5-agent review 一致認為「precision doesn't compound to better cypher, just adds branching agents must trial-and-error through」 |
| **B.1** | 單一 HasMethod，target kind 不限 | **選定** — 單一 predictable pattern, LLM 學 1 line cypher 慣例就全語言 work |

### 11.2 5-agent review 結論

5 個 Haiku agent 在無 prior context 下獨立評估 B.1 vs B.4：

- 5/5 選 B.1
- 共識 1：LLM agents reliability favours single predictable pattern over conditional logic
- 共識 2：B.4 的兩 edge type 「splits the same semantic concept across two relation names, forcing agents to learn arbitrary language-specific routing rules」
- 共識 3：`cgn inspect Class` is the primary surface, already hides heterogeneity — cypher 不該重複表達精度

詳細評估 prompt 與回應記錄於 PR description（無 prior bias，純獨立 review）。
