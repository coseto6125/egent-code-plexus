# Parity Verification Protocol

跨實作（cgn-rs ↔ ref-gitnexus）的 symbol emission 比對流程。本文件描述
**怎樣的證據才能斷定 parser 有 bug**、以及 **怎樣的證據只是 taxonomy 差**。

目標讀者：要在同樣 corpus 上做 parity 工作的 model / 開發者。

---

## 0. 為什麼要這份文件

`.sample_repo` 有 14 種語言混合 source，是 cgn-rs 和 ref-gitnexus 之間
的 A/B oracle。但「兩邊算出來的 symbol 數量不同」並不直接等於「parser
有 bug」。常見幾種「看起來像 bug 但其實不是」的差異：

1. **label diff** — 同 `(path, name)` 兩邊都有 emit，但 kind 不同
   (`Method` ↔ `Function`、`Property` ↔ `Variable`)。屬 taxonomy choice。
2. **design choice** — 一邊故意 drop 某類 symbol（例如 function-local
   variable、inner fn）。屬 product design，非 bug。
3. **harmless defensive** — query 加了 `(#not-eq?)` 之類防禦判斷，無實
   證 bug，只是 future-proof。屬 cosmetic，非 bug。
4. **真 bug** — 兩邊在同 `(path, name)` 完全沒對應 row、且該 symbol
   屬於語義上「永久的、type-level 命名實體」。這才需要修 parser。

抽樣前先掌握 4 類分流，才不會做白工。

---

## 1. 核心方法論

### 1.1 dump 三元組

對每個 lang 從兩邊各 dump `(kind, filePath, name)` 三元組成集合：

- cgn-rs：用 cypher query 走 `~/.cgn/<repo>__*` 索引
- ref-gitnexus：用其 Python API 走 `.sample_repo/<Lang>/...`

工具：`scripts/parity/dump_per_lang_symbols.py`

dump 完落地成兩個 set：
- `symbol_diffs/<Lang>_rs_only.txt`  ← cgn-rs 有、ref 沒有
- `symbol_diffs/<Lang>_ref_only.txt` ← ref 有、cgn-rs 沒有

### 1.2 file-extension scoping（重要陷阱）

早期版本用「dir-prefix scoping」(`STARTS WITH 'Java/'`) 來分 lang，遇到
兩個問題：

1. `Java/` 會誤命中 `JavaScript/...`（前綴衝突）
2. Kotlin 樣本 repo 是 mixed-Kotlin/Java，dir prefix 會把 `.java` 檔當
   成 Kotlin emit

當前 dump 改成 **file-extension scoping**（`ENDS WITH '.ts' OR '.tsx'`），
針對 root index 而非 per-lang sub-index。修正後 ref real_count 可下降
50%（先前是 partial index + cwd 雙重 bug 造成的虛胖）。

### 1.3 aggregator：cross-side pairing

直接看 `_only.txt` 的 raw 行數會誤判。例如：

```
rs_only.txt:  Method   benches/copy.rs  poll_flush
ref_only.txt: Function benches/copy.rs  poll_flush
```

這兩邊指同一個 declaration，只是 kind label 不同 → 應歸類為
**label_diff**，不該算進 real gap。

工具：`scripts/parity/parity_aggregate.py`

> **重要陷阱 — aggregator 必須讀 full set，不能只讀 `_only.txt`**：
> 如果 rs 邊有 `(Function, p, at)` 在 common（兩邊都是 Function），ref 邊額外
> 有 `(Template, p, at)` 在 ref_only，那 rs_only 不含 `at` 任何條目。
> aggregator 若只看 `_only.txt` 就會看不到 rs 邊的 Function row，誤判
> `(Template, p, at)` 為 real ref_over。dump script 必須額外輸出
> `<Lang>_rs_all.txt` / `<Lang>_ref_all.txt`（完整 set），aggregator 用 full
> set 跨 kind pair，才能正確扣除 label_diff。

aggregator 用 **EQUIV class 等價類**做 cross-side pairing：

```python
_EQUIV_CLASSES = [
    {"Method", "Function", "Template", "Constructor"},
    {"Typedef", "TypeAlias"},
    {"Const", "Variable", "Property", "Static"},
    {"Interface", "Struct", "Enum", "Annotation", "Class"},
    {"Delegate", "Function"},
]
```

union-find 合併重疊類（`{Delegate, Function}` 透過 `Function` 跟
`{Method, Function, Template, Constructor}` 合併成一大類），保留跨類
傳遞性。

aggregator 輸出按 `(path, name)` 配對：
- 兩邊都有 + kind 同 equiv class → `label`
- 一邊獨有 + kind 在 model-only set（如 rs 的 `EntryPoint` 是 cgn-rs
  獨有 NodeKind）→ `model`
- 其餘 → `real_rs` / `real_ref`，**這才是真正需要驗的 candidate**

### 1.4 抽樣協議

挑一個 `real_ref` 高量 kind 作 candidate（例如 Rust Function 47 個
unpaired），按 **unique-name 字母順序** 取前 10 個樣本。**順序固定，禁
止 cherry-pick**。

對每個樣本：

1. `grep -nE "fn\s+${NAME}\b"` 找 declaration line
2. 看 enclosing context：是 module-level / type-level 還是 block-level
3. 對照 cgn-rs parser 設計：是 design choice drop 還是 parser gap

判定規則（**這條規則本身要寫死，不准每輪換**）：

| 樣本性質 | 判定 |
|---|---|
| function/method body 內的 transient declaration（local var、inner fn） | **ref over-emit, cgn-rs design choice** |
| module-level 但被 arrow-fn 賦值的 const (`const X = () => {...}`) | **ref over-emit**（cgn-rs 把它 emit 成 Function 而非 Const） |
| type-level permanent 命名實體（struct field、enum variant field、impl method） | **若 cgn-rs 沒 emit 則為真 cgn-rs bug，要修 parser** |
| Test/Reference 路徑下被 builder filter 掉 | **design choice**（`is_non_production` filter） |

10/10 全 ref-bug → 該 candidate 通過為 design choice/label diff，不修。
中途有一個是 cgn-rs bug → **立即停下、進入修補階段、reset round 計數**。

---

## 2. 工具腳本清單

| 路徑 | 用途 |
|---|---|
| `scripts/parity/dump_per_lang_symbols.py` | dump 兩邊 (kind, path, name) 三元組為 `_only.txt`。支援單 lang 或全 14 lang。 |
| `scripts/parity/parity_aggregate.py` | 讀 `_only.txt`、跑 cross-side pairing、印 model/label/real_rs/real_ref 表。 |
| `scripts/parity/symbol_diffs/<Lang>_*.txt` | dump 落地檔案。reindex 後重 dump 會覆蓋。 |

`find_unpaired*.py` 是抽樣輔助腳本，要找特定 kind 的 unpaired
candidate 時新建一份小腳本即可：

```python
# /tmp/find_unpaired.py — 找 Rust 邊 Function unpaired ref_over
from pathlib import Path
DIFF = Path("scripts/parity/symbol_diffs")
EQUIV = {"Method", "Function", "Template", "Constructor"}

rs_pn = set()
for line in (DIFF / "Rust_rs_only.txt").read_text().splitlines():
    p = line.split("\t", 2)
    if len(p) == 3 and p[0] in EQUIV:
        rs_pn.add((p[1], p[2]))

unpaired = []
for line in (DIFF / "Rust_ref_only.txt").read_text().splitlines():
    p = line.split("\t", 2)
    if len(p) == 3 and p[0] == "Function" and (p[1], p[2]) not in rs_pn:
        unpaired.append((p[1], p[2]))

print(f"Total unpaired Rust Function ref_over: {len(unpaired)}")
for path, name in unpaired[:20]:
    print(f"  {name:<30} @ {path}")
```

---

## 3. 完整工作流

從 fresh state 開始的一輪驗證循環：

```bash
# Step 1: 確保兩邊都已 index 同一份 .sample_repo
cgn admin index --repo /home/enor/gitnexus-rs/.sample_repo --force
# (ref-gitnexus 那邊有自己的 indexer，按 ref repo 文件)

# Step 2: dump 兩邊 symbol 集合
cd /path/to/cgn-rs/worktree
python3 scripts/parity/dump_per_lang_symbols.py    # 全 14 lang
# 或單 lang:
python3 scripts/parity/dump_per_lang_symbols.py Rust

# Step 3: 跑 aggregator 看真實 gap
PARITY_DIFF_DIR=scripts/parity/symbol_diffs \
    python3 scripts/parity/parity_aggregate.py

# 看 top_real_gap 欄。挑一個 candidate kind 開新一輪。

# Step 4: 找 unpaired candidate（針對選定 kind 寫小腳本）
python3 /tmp/find_unpaired.py   # 自己寫，跟 EQUIV class 同步

# Step 5: 抽前 10 個 unique-name 樣本，逐一 grep declaration
for spec in "...:..." ...; do
    P="${spec%%:*}"; N="${spec##*:}"
    grep -nE "fn\s+${N}\b" "$SAMPLE_REPO/$P"
done

# Step 6: 看 enclosing context 分類
# - 全是 block-scoped transient → design choice → 通過
# - 出現 type-level permanent → cgn-rs bug → 進入修補

# Step 7（若有 bug）: 修 parser 並寫 regression test
$EDITOR crates/cgn-analyzer/src/<lang>/queries.scm
$EDITOR crates/cgn-analyzer/tests/<lang>_<dimension>.rs
cargo test -p cgn-analyzer --test <lang>_<dimension>

# Step 8: 重 index + 重 dump + 重 aggregate 確認 real_ref 下降
cgn admin index --repo /path/.sample_repo --force
python3 scripts/parity/dump_per_lang_symbols.py <Lang>
PARITY_DIFF_DIR=scripts/parity/symbol_diffs python3 scripts/parity/parity_aggregate.py
```

---

## 4. 案例：Rust enum variant struct-form field（185 → 0）

完整一輪「發現 → 修復 → 驗證」流程的真實 trace。

### 4.1 發現

aggregator 顯示 Rust real_ref = 232，top: `Property-185, Function-47`。
185 個 Property 是 ref 邊獨有、cgn-rs 邊在同 `(path, name)` 沒任何
equiv class kind。

### 4.2 抽樣

按 unique-name 字母順序取前 10：

| # | name | path |
|---|---|---|
| 1 | key | Rust/examples/tinydb.rs |
| 2 | msg | Rust/examples/tinydb.rs |
| 3 | previous | Rust/examples/tinydb.rs |
| 4 | value | Rust/examples/tinydb.rs |
| 5–10 | `_function_name`, `_module_address`, ... | move/aptos-move/.../sdk_builder.rs |

### 4.3 grep + context

```rust
// tinydb.rs L63-83
enum Request {
    Get { key: String },
    Set { key: String, value: String },
}

enum Response {
    Value { key: String, value: String },
    Set { key: String, value: String, previous: Option<String> },
    Error { msg: String },
}
```

10/10 全是 `enum_variant` 的 `field_declaration_list` 內的
`field_declaration`。

### 4.4 判定

關鍵問題：這算 design choice 還是 cgn-rs bug？

對比已知 design choice：
- TS function-local const drop（block-scoped, transient）
- Java method-local Variable drop（block-scoped, transient）
- Rust inner fn drop（block-scoped, transient）

enum variant struct-form field 是 **type-level, permanent**：
- 在 type 定義內，跟 struct field 平行
- pattern-match destructure `V { f1, f2 } => ...` 直接依賴 name
- ref-gitnexus 一致 emit 為 Property

→ **判定為真 cgn-rs gap**，須修 parser。

### 4.5 修補

確認 tree-sitter-rust grammar：`enum_variant body: field_declaration_list`
與 `struct_item body: field_declaration_list` 同結構，可以套同一 capture。

`crates/cgn-analyzer/src/rust/queries.scm`：

```scheme
;; Enum variant struct-form fields: `enum E { V { f1: T, f2: U } }`. Each named
;; field is a permanent type-level data member parallel to struct fields, and
;; pattern-match destructuring `V { f1, f2 } => ...` references them by name.
(enum_variant
  body: (field_declaration_list
    (field_declaration
      (visibility_modifier)? @export
      name: (field_identifier) @property.name) @property))
```

### 4.6 regression test

`crates/cgn-analyzer/tests/rust_enum_variant_fields.rs`：

```rust
#[test]
fn single_variant_field_emits_property() {
    let g = parse("enum E { V { f1: i32 } }");
    assert_eq!(properties(&g), vec!["f1"]);
}

#[test]
fn tuple_variant_emits_no_property() {
    let g = parse("enum E { V(i32, String) }");
    assert!(properties(&g).is_empty());
}

// 共 6 cases：single / multi / no-dup / tuple-empty / unit-empty / mixed
```

### 4.7 驗證

- `cargo test -p cgn-analyzer` → 245+6 tests pass，無 regression
- reindex + 重 dump + 重 aggregate：
  - Rust Property real_ref: **185 → 0** ✓
  - Rust 整體 real_ref: 232 → 59（淨減 173）
  - Property 從 `top_real_gap` 消失

收斂確認。

---

## 5. 失敗模式與經驗教訓

### 5.1 不要用「raw `_only.txt` 行數」當 ref-over 指標

`_only.txt` 是 per-kind set diff，會把 `Method/Function` 同 declaration
的 label diff 算進「ref over-emit」。**必須 aggregator pair 之後看
`real_ref`/`real_rs` 才是真實 gap**。

範例：raw Rust Function ref_only 2498 → aggregator pair 後 real_ref 只剩 47。

### 5.2 不要用「總數接近 ref」當 accuracy 指標

「cgn-rs 比 ref 多 17000 個 symbol」不等於「cgn-rs 找到更多 symbol」。
可能是 label policy 差（Property = field 的展開）、EntryPoint markers、
重複 emit 等等。**只有 per-symbol verification 才能講 accuracy**。

### 5.3 抽樣前先讀 grammar / queries

抽樣判定要對照 parser 設計才能講「design choice」還是「bug」。例如：

- Rust queries.scm 把 `source_file > function_item` 跟 `mod_item > declaration_list > function_item` 限定為 module-scope anchor → inner fn 不 capture 是 by design
- 同樣 queries.scm 對 struct field 有 capture 但對 enum variant field 沒 capture → 那是漏抓（grammar 本身 enum_variant body 就是 field_declaration_list，完全可以套同一 pattern）

### 5.4 修補要寫 regression test，不是只跑 dump 對數

dump 數對得上不代表 parser 行為正確；可能對巧合 sample 才對。**unit
test 直接 parse 短字串、檢查 NodeKind 才是 ground truth**。

### 5.5 parse_cache 陷阱

cgn-rs 有 `parse_cache`（fingerprint = `"v" + CARGO_PKG_VERSION + "+schema1"`），
重 build binary 後 cache 仍指 old behavior。修 parser 後必 reindex with
`--force`：

```bash
cgn admin index --repo /path --force
```

PR #132 修了 `--force` 不清 parse_cache 的 bug；舊版本可能要手動
`rm -rf ~/.cgn/<repo>__*/parse_cache`。

### 5.6 round 紀律

每輪只一個目標。不要同時改 static_item + Property + Delegate + Const，
無法歸因哪個 fix 對應哪個 real_ref 下降。**修完一個 candidate、重跑
aggregator 確認該 kind real_ref 歸零、再開下一輪**。

中途若發現 cgn-rs bug，**reset round 計數**。連續 10 輪 ref-bug 才能宣
告該 candidate 為 design choice / 通過。

---

## 6. 4 類分流速查

| 類別 | 證據 | 動作 |
|---|---|---|
| **真 bug** | type-level permanent symbol，cgn-rs 在同 (path, name) 完全沒對應 row | 修 parser + 寫 regression test + 重驗 |
| **label diff** | 同 (path, name) 兩邊都有 emit、kind 在 EQUIV class 內 | aggregator 自動處理，不改 parser |
| **design choice** | block-scoped transient / 跨 lang 一致的 model 取捨 / builder filter（如 Test/Reference Route filter） | 記入文件，不修 |
| **無害 defensive** | query predicate guard，沒實證 bug | 視情況保留或 revert，不影響 parity |

---

## 7. 跟其他 model 對接

要在不同 model（minmax2.7 等）上做同一輪驗證：

1. **clone 工具**：複製 `scripts/parity/dump_per_lang_symbols.py` 與
   `scripts/parity/parity_aggregate.py`
2. **同步 EQUIV class**：兩邊 aggregator 的 `_EQUIV_CLASSES` 必須一致，
   否則 label_diff 歸類會不同
3. **固定樣本順序**：用 `awk -F'\t' '!seen[$3]++'` 取 unique-name 後按
   字母序，禁止 cherry-pick
4. **固定判定規則**：本文 §1.4 表格，不准每輪換
5. **修補完整流程**：必經 grammar 確認 → parser fix → unit test →
   reindex --force → 重 dump → 重 aggregate 驗 real_ref 下降

每輪輸出 checklist：
- [ ] candidate kind 與 sample count
- [ ] 10 個樣本的 (file, line, declaration scope)
- [ ] 判定結果（每個樣本標 design / bug / label / defensive）
- [ ] 若有 bug：parser diff + regression test diff + reindex 後 aggregator real_ref 變化
- [ ] round 計數（連續 ref-bug 達 10 才算通過）
