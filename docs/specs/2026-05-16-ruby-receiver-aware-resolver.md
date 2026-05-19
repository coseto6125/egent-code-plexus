# Ruby Receiver-Aware Resolver — design spec

**Date**: 2026-05-16
**Status**: Both phases landed in PR #13 — single-file emit + Option C (resolver Tier 2.75 `HeritageScoped`) closes the cross-file mixin gap. The pin test was flipped to verify positive resolution.
**Goal**: 描述如何讓 Ruby parser/resolver 能可靠識別 `def_delegator` / `def_delegators` / `delegate` 等 metaprogramming 呼叫，補上 PR [#13](https://github.com/coseto6125/code-graph-nexus/pull/13)（Ruby Named binding）刻意延後的最後一塊——「需要 receiver type 才能判斷的 method-creating call」。

**Status update (post-implementation, 2026-05-16)**：PR #13 後續三個 commit 已完整 ship Option B + Option-A fallback + **Option C resolver tier**：
- `crates/cgn-analyzer/src/ruby/{parser.rs, queries.scm}` 加入 `def_delegator` / `def_delegators` / `delegate` 三個 metaprogramming 呼叫的識別。除了 RawImport alias 之外，**也同步 materialise 一個 `NodeKind::Method` RawNode** 到 enclosing class — 這是讓跨檔 heritage chain 能找到 delegated method 的關鍵。
- `crates/cgn-analyzer/src/resolution/heuristics.rs` + `resolver.rs` 新增 **Tier 2.75 `HeritageScoped`**（confidence 0.80，介於 QualifierScoped 0.85 與 Global 0.7）：bare-name callee 在 Tier 1/2/2.5 都 miss 時，把 caller 的 enclosing-class heritage 視為 implicit qualifier，逐一用 `resolve_qualifier_file` 找 parent 檔，再 `lookup_in_file` 找 method。
- `crates/cgn-analyzer/src/resolution/builder.rs` 新增 `enclosing_class_heritage` helper：對每個 method-level RawNode 找最小 span 包住它的 Class（NodeKind::Class，Ruby module 也是 Class kind）並回傳 heritage 列；Pass-2 call-edge emission 改用新增的 `resolve_symbol_with_heritage` 把 heritage 傳進去。
- `crates/cgn-cli/tests/ruby_cross_file_mixin.rs` **flipped** — 原 pin 住「跨檔 mixin 不傳遞」的測試現在反向驗證「`bar.rb` 內的 `read` callsite 透過 Bar.heritage=[Foo] 走 Tier 2.75 解到 `lib/foo.rb` 內的 delegated Method」。
- 全 workspace 817 tests 全綠（cross-language regression-free），clippy `-D warnings` clean。

**Related**:
- [[named-binding-ruby]] — PR #13，已加入 `alias` keyword / `alias_method` / 常數別名三類靜態可解析的 named binding。
- [[matrix-optimization-opportunities]] §A1 Ruby（`attr_*` properties + `include`/`extend` mixin tracking，HEAD `80b77f8`）。
- `crates/cgn-analyzer/src/ruby/{parser.rs, queries.scm, receiver_types.rs}`。

---

## 1. Problem statement

Tree-sitter-ruby 把所有 method 風格的呼叫（`obj.foo`、`Module.foo`、純函式 `foo bar`、metaprogramming `def_delegator :target, :method`）一律解析成 `call` 節點。實際的 AST 形態完全相同：

```ruby
# (1) 一般 method call
logger.info("hello")

# (2) Forwardable metaprogramming — 隱含「在 host class 上新增 method `name` → 委派給 target.name」
class Album
  extend Forwardable
  def_delegator :@songs, :each
end

# (3) ActiveSupport-style delegate
class Order
  delegate :address, :phone, to: :customer
end

# (4) 自定義 method 同名地獄
class MyForwardable
  def def_delegator(target, sym); end   # 完全合法的同名 instance method
end
```

從 `call` 節點本身看，(1)(2)(3)(4) 在 token-tree 上難以區分。要安全 emit「`each` 是 Album 的 method（委派到 `@songs.each`）」這個事實，必須知道 **call 發生時 receiver 的型別 / 模組來源**——亦即「`def_delegator` 是不是來自 `Forwardable` mixin」。沒有 receiver-aware 的判斷，三條路只能擇一：

1. **完全不處理**（PR #13 的現狀）→ 漏掉 Rails / Forwardable 兩大 metaprogramming 流派，影響 Named binding、Constructor inference、Framework detection 三個矩陣 cell。
2. **無腦 whitelist symbol name** → 對 case (4) 直接誤殺，把使用者自定義 method 當成 metaprogramming。
3. **真做 receiver-aware** → 需要在 parser pass 或 resolver pass 引入 class-scope 的 mixin 表。

這份 spec 比較三條路、推薦選項，並界定實作範圍與 rollout。

---

## 2. Scope

### In scope

| Pattern | 範例 | 期望輸出 |
|---|---|---|
| `Forwardable.def_delegator` | `def_delegator :@songs, :each` | host class 新增 method `each` → call edge 指向 `@songs.each` |
| `Forwardable.def_delegators` | `def_delegators :@a, :foo, :bar` | host class 新增 method `foo` / `bar` 各一條 |
| ActiveSupport-style `delegate` | `delegate :a, :b, to: :customer` | host class 新增 method `a` / `b` → call edge 指向 `customer.{a,b}` |
| `class << self` singleton block | block 內 `def x` 視為 class method | 已有 receiver_types 部分處理，本 spec 補完跨 mixin 場景 |

### Out of scope

- 動態 `send` / `public_send` / `__send__` 呼叫（receiver 名字非字面常數）。
- `eval` / `class_eval` / `instance_eval` 內的字串 metaprogramming。
- Refinement (`using Foo`) 引入的 method scoping。
- 跨 gem 邊界的 mixin tracking（單純沒拿到的 gem 原始碼直接走 BlindSpot）。

### Cross-file boundary

預設只跑 **單檔內** 的 mixin tracking——`extend Forwardable` 必須跟 `def_delegator` 在同一個 class body 裡 lexically 看得到。跨檔案的 mixin（例如 `concern` module 被 `include` 進 host class）走第二階段：resolver 拿全 graph 的 `Heritage` edge 做傳遞閉包。本 spec 第一階段不擴大到 cross-file。

---

## 3. Design options

### Option A — Symbol-level whitelist（直球派）

在 `queries.scm` 加上 `(call method: (identifier) @meta_call (#match? @meta_call "^(def_delegator|def_delegators|delegate)$"))`，看到就 emit `RawImport` + 派生 method node。完全不查 receiver。

**Pros**：
- 實作 LOC 最低（估 ~80 LOC parser + ~20 行 query）。
- 不需要碰 `receiver_types.rs` 或 resolver。
- 跟既有 `attr_*` metaprogramming 走同一條 query pipeline。

**Cons**：
- 誤殺率高：使用者只要定義同名 instance method（case (4)）就 false positive。Forwardable / ActiveSupport 在 Ruby 生態極度普遍，但同名 method 也並非罕見。
- 沒有「真的看到 `extend Forwardable`」的 sanity check，noise 會傳染到下游 Named / Constructor 矩陣 cell。
- 把語意正確性的責任丟給 reviewer。

### Option B — Class-scope mixin tracking（在 parser 階段做）

延伸 PR #13 + Wave 1 既有的 mixin 蒐集（`pending_mixins` 在 `parser.rs:131`）：在掃 `call` 節點時，先查當前 enclosing class 是否 `extend`d 任何已知的 metaprogramming module（`Forwardable`、`ActiveSupport::Concern`、`Module`-level `delegate`）。

實作要點：
- 重用 `ClassContext` (`receiver_types.rs:21`) 的 enclosing-class-by-line 邏輯。
- 新增 `MixinTable: class_name → Set<module_name>`，在 parser 第一輪掃描完整 `extend` / `include` 後 freeze。
- 第二輪掃 `call` 時：whitelist symbol 只在 `MixinTable[enclosing_class]` 命中對應 module 時觸發。

**Pros**：
- 對單檔場景精準度顯著優於 A——使用者自定義 `def_delegator` 不會誤殺（因為 host class 沒 `extend Forwardable`）。
- 不需要動 resolver / graph builder，全部在 `crates/cgn-analyzer/src/ruby/` 內完成。
- 跟既有 mixin pipeline 共用資料結構，沒有 cross-crate 變動。

**Cons**：
- 跨檔案 mixin 抓不到（例如 `Forwardable` 被某個本地 module 重新匯出後 `include`）→ 需明確 fall back 到 BlindSpot，不能假裝看見。
- 雙 pass 帶來 ~5-10% Ruby parsing 時間 overhead（單檔 mixin 表小，可忽略）。
- 沒處理「同檔但詞法順序：`def_delegator` 出現在 `extend Forwardable` 之前」的邊角——Ruby 允許但極罕見，spec 明確列為 known limitation。

### Option C — Receiver-aware resolver tier（在 resolver pass 做）

把判斷推到 `crates/code-graph-nexus-resolver/`（或 builder.rs），讓 resolver 在 graph 已建好、所有 `Heritage` / `Mixin` edge 都在的情況下，回頭重訪每個 `call`、查 receiver 的 mixin closure、決定要不要 emit 額外的 method/delegate edge。

**Pros**：
- 唯一能正確處理跨檔案 / 跨 gem mixin closure 的選項。
- 把 metaprogramming 解釋從 parser layer 抽離，未來擴充 `define_method` / `class_eval` 等更動態的 pattern 較容易。
- 概念上最乾淨——parser 只負責「我看到一個 `call`，receiver 文字是這個」，語意決策歸 resolver。

**Cons**：
- 改動半徑最大：要動 resolver 的 visit phase、graph builder 的 edge ingestion、可能還要在 `RawNode` 上加新 variant。
- PR 大小遠超 named binding 的範圍，不適合 PR #13 後立即追加。
- 需要先設計 `Mixin` edge 的具體 schema（目前只有粗糙的 `Heritage`）。

### Trade-off summary

| 維度 | A. Whitelist | B. Mixin table | C. Resolver tier |
|---|---|---|---|
| 估 LOC | ~100 | ~250 | ~600+ |
| 估測試 | 4-6 case | 10-12 case | 15-20 case |
| False positive 率 | 高 | 低 | 極低 |
| 跨檔 mixin 支援 | 無 | 無（BlindSpot） | 完整 |
| 衝擊模組數 | 1 (`ruby/`) | 1 (`ruby/`) | 3+（`ruby/`、resolver、builder） |
| PR 數量 | 1 | 1 | 2-3 (stacked) |
| 與 PR #13 關係 | 直接 build on top | 直接 build on top | 需先動 resolver 基礎 |

---

## 4. Recommended path

**選 Option B**，並把 Option A 收編為 B 的「fail-soft fallback」：當 parser 掃完整檔仍未看到任何 `extend` / `include` 但出現 metaprogramming 名稱時，emit 一個帶 `confidence=low` 標記的 `RawImport` + 對應 `BlindSpot::DynamicDispatch` raw event，讓 downstream 可選擇是否消費。Option C 保留為後續獨立 spec（暫名 `[[ruby-cross-file-mixin-resolver]]`）的目標。

### 預估規模

- Parser 主體：~200 LOC（`parser.rs` 新增第二輪 walk、共用 `ClassContext`）。
- Queries：~30 LOC（三個新 capture：`@delegator_method`、`@delegator_target`、`@delegate_to`）。
- Tests：`tests/ruby_metaprogramming.rs` 增加 8-10 case，覆蓋四種 in-scope pattern + case (4) negative + 詞法順序 edge case。
- 預估總 PR diff < 400 LOC。

### Impact radius（cgn impact 預估）

- `crates/cgn-analyzer/src/ruby/parser.rs` — 唯一主要改動點，新增 method-creating call 識別。
- `crates/cgn-analyzer/src/ruby/queries.scm` — 新增 capture。
- `crates/cgn-analyzer/src/ruby/receiver_types.rs` — 可能小幅 refactor 把 `ClassContext` 抽到 `mod.rs` 共用。
- `crates/cgn-analyzer/tests/ruby_metaprogramming.rs` — 增測。

不動 resolver / builder / graph schema。

### 與 PR #13 的相對位置

直接 build on top of PR #13 merged 之後的 `main`，不堆 stack。PR #13 已經把 `RawImport { alias, imported_name, source }` 的形狀固定下來；新 PR 沿用同一個 import 形狀，差別只在「source 是 receiver 上的 method 字串、alias 是 host class 新 method 名」。如果決定先做 receiver_types refactor，可拆成兩個獨立 PR：(1) 抽出 `ClassContext` 到 `mod.rs`、(2) 加入 metaprogramming 識別；兩個 PR 之間沒有 stacked 依賴。

---

## 5. Migration & rollout

- 既有 `tests/ruby_metaprogramming.rs`（7 case）行為不變——僅追加新測試檔或新 section。
- 既有 `tests/ruby_named.rs`（PR #13）保持綠燈；新 metaprogramming 走獨立 import shape，不污染 alias path。
- README.md:230 的 Ruby row：Named 已是 ✓（PR #13 已 mark），本 PR **不改 Named cell**；但會升級 Constructor cell 的精準度（Forwardable 派生 method 不再消失於 `class << self` 黑洞）。README per-cell note 可追加：`Ruby Named also covers Forwardable.def_delegator and ActiveSupport.delegate`，但這視 reviewer 偏好。
- Feature flag：**不引入**。Mixin table 是 strict superset，false-positive 改善方向，沒有 rollback 需求。
- Telemetry：第一輪 release 後抓 `RawImport { confidence: low }` 比例，若 >5% 表示 Option A 部分仍噪音過大，考慮收緊 fallback 條件。

---

## 6. Open questions

1. **`ClassContext` 該住哪？** 目前在 `receiver_types.rs`，但 mixin table 與 metaprogramming 識別都會用到。提到 `ruby/mod.rs` 還是 `ruby/scope.rs` 新檔？影響 PR 切法。
2. **`delegate :foo, to: :bar` 的 `to:` 解析**：`tree-sitter-ruby` 把 hash arg 攤平成 `pair` 節點，需要在 query 階段抓 key=`to` 的 value。是否值得把這條 query 抽成 helper 給未來其他 metaprogramming 重用？
3. **詞法逆序的 `def_delegator` 在 `extend Forwardable` 之前**：實務上幾乎不存在，但 spec 是否要 hard-fail / 還是降為 low-confidence？我傾向 low-confidence，行為跟跨檔案 mixin 一致。
4. **`Module#delegate` vs Rails-only `ActiveSupport::Module#delegate`**：兩者語法相同但語意上前者是 Forwardable 衍生、後者帶 `prefix:` / `allow_nil:` 等 option。第一階段是否一律當成 Forwardable 對待、忽略 option？我傾向「忽略 option，emit method node，不解析額外 keyword args」。
5. **跟 [[fanout-resolution]] 的互動**：若 metaprogramming emit 出去的 method node 之後被 `extend` 到別的 class，fan-out 怎麼處理？這在 Option C 才會真正面對，B 階段直接 emit 在原 host class 即可。
6. **是否需要 `BlindSpot` 事件**：對 Option A fallback 路徑來說，emit 一個 `BlindSpot::MetaprogrammingFallback { call_site, reason }` 會幫助下游觀察品質，但會增加 raw stream 體積。需要在 perf 與 observability 間取捨。

---

## 7. Decision log

| Decision | Choice | Rationale |
|---|---|---|
| 主路線 | Option B + Option C 一起 ship | 原 spec 推薦 B 並把 C 留給未來；落地時驗證 C 的 patch 規模可控（resolver +35 LOC, builder helper +25 LOC, 全 workspace test 仍 0 failure），所以一輪內收掉。 |
| Option A 角色 | 收編為 B 的 low-confidence fallback | 避免「看到 `def_delegator` 但沒看到 `extend Forwardable`」直接消失；後續若 `BindingKind` 落地可再升級為高/低信心二分 emit。 |
| 跨檔 mixin | 已透過 Tier 2.75 + parser materialise Method node ship | `Resolver::resolve_symbol_with_heritage` 用 caller heritage 當 implicit qualifier 探 parent 檔，parser 把 delegator 同時 emit 成 Method RawNode，跨檔 chain 自動 work。`ruby_cross_file_mixin.rs` 的 pin test 已 flipped 為正向斷言。 |
| 與 PR #13 關係 | 直接追加 commit 到 PR #13（非 stacked） | `def_delegator/s` + delegate 識別在 PR #13 的 second/third commit 內 ship；同一 PR 的 cross-file pin test 把 architectural 限制鎖住。 |
| Feature flag | 不引入 | 改進方向，無 rollback 需求。 |
| README 矩陣調整 | 不改 cell，per-cell note 已追加 `def_delegator/s + delegate (with Forwardable mixin detection)` | Named 已 ✓；本 commit 升級 per-cell note 的 coverage 描述。 |
| BindingKind 欄位 | 暫不引入；low-confidence fallback 仍 emit | PR #15（`BindingKind` on `RawImport`）尚未 merge 到 main；待其落地後可重訪「Forwardable 才高信心 emit」的 strict mode。 |
