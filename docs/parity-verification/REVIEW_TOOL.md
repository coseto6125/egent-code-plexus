# Per-symbol parity review packet — `review_diffs.py`

A companion to [`README.md`](README.md) §1.4 抽樣協議。Where the
aggregator (`parity_aggregate.py`) tells you **how many** diff entries
each lang/kind has, this tool dumps the **per-symbol** review packet so
you can walk through them one-by-one without re-typing `grep -nE
"…"` 14 times per language.

For each diff entry the packet shows:

1. 哪邊 emit 了什麼 (`cgn emits: Class` / `ref-gitnexus emits: —`)
2. `.sample_repo/<Lang>/<path>` 該 declaration 的源碼，以 first
   `\b<name>\b` 為中心 ±N 行 context（grep heuristic — 不是 parser-grade
   line resolution，但夠快、夠準）
3. 一條空白 `**Verdict**: _____` 由 reviewer 自己填
   `real_bug / label_diff / design / defensive`

兩邊跑同一份 `.sample_repo`，源碼一致；diff 的是「每邊 parser 對同個
declaration 各 emit 了什麼 NodeKind」。所以 packet 只附一份源碼 + 兩邊
emission，而非「前後檔案 diff」。

---

## 0. 前置條件

跑這隻腳本前，先確認 `scripts/parity/symbol_diffs/<Lang>_*_all.txt`
是最新的。流程：

```bash
# 1. 兩邊都 index 同一份 corpus
cgn admin index --repo /home/enor/code-graph-nexus/.sample_repo --force
# (ref-gitnexus 端按其文件 index)

# 2. dump (kind, path, name) 三元組
python3 scripts/parity/dump_per_lang_symbols.py            # 全 14 lang
python3 scripts/parity/dump_per_lang_symbols.py TypeScript # 單 lang

# 3. 看 aggregator 整體 gap 表
PARITY_DIFF_DIR=scripts/parity/symbol_diffs \
    python3 scripts/parity/parity_aggregate.py

# 4. 用 review_diffs.py 抽 packet（本文重點）
python3 scripts/parity/review_diffs.py --lang PHP
```

跳過 1-2 直接跑 review 也行 — 它讀的就是步驟 2 產出的檔案。

---

## 1. CLI 介面

```text
python3 scripts/parity/review_diffs.py [options]

  --lang     <Lang>                    14 lang 之一，省略 = 跑全部
  --kind     <NodeKind>                只挑一個 kind（例 Function、Method）
  --bucket   real_rs,real_ref[,label]  default: real_rs,real_ref
  --limit    N                         每 (lang, kind) 最多 N 筆，0=不限。default 50
  --context  N                         源碼上下文 ±N 行。default 10
  --out-dir  <path>                    輸出位置。default scripts/parity/review/
```

**設計哲學：只列「真正不一致」的差異，一致 / EQUIV 已配上的不收。**
review packet 的目的是把人類眼睛聚焦在還需判斷的 entry，預設兩 bucket
都是「兩邊在同 `(path, name)` 對不上」的 row；label / model 因為已自動
分類為「同 declaration、不同 label」或「設計差異」，預設不進 packet。

`real_rs`：cgn 有 emit、ref-gitnexus 同 `(path, name)` 沒有任何 EQUIV
class kind。即「cgn over-emit 或 ref-gitnexus under-emit」。**收進
packet（default）**。

`real_ref`：相反方向。即「ref 有、cgn 沒抓到」— 通常是 cgn parser
gap candidate（要修的目標）。**收進 packet（default）**。

`label`：兩邊都有同 `(path, name)`，但 kind 不同（且都落在同一 EQUIV
class）。**預設不收** — aggregator 已歸 label_diff，源碼一致、只是 kind
標籤差，不需要逐筆人工裁決。只在 audit EQUIV class 是否漏編組時用
`--bucket label` 抽樣。

`model`：rs / ref 各自獨有的 NodeKind（如 rs 的 `EntryPoint`、ref 的
`Section`），永遠不會 pair。**預設不收** — 屬設計差異，不修。

---

## 2. 典型用法

### 2.1 一輪驗證：聚焦單一 candidate kind

aggregator 顯示「TS Function real_ref 221」，要驗。

```bash
python3 scripts/parity/review_diffs.py \
    --lang TypeScript --kind Function --bucket real_ref --limit 20 --context 12
```

輸出 `scripts/parity/review/TypeScript_review.md`，含 20 筆 ref_over
Function entries，按 unique `(path, name)` 字母序。逐條走、填 verdict。

10/10 design → 通過 candidate。
中途出現 type-level permanent → 立即停下、開新 round 修 parser、reset
round 計數（README.md §5.6）。

### 2.2 PR 前後對照

剛修了 PHP const capture，想看修完後 PHP 還剩什麼 ref_over：

```bash
python3 scripts/parity/dump_per_lang_symbols.py PHP        # 重 dump
python3 scripts/parity/review_diffs.py --lang PHP --bucket real_ref --limit 0
```

`--limit 0` 不設 cap、全 dump，packet 完整反映 reindex 後狀態。

### 2.3 同時看 rs_over + ref_over

```bash
python3 scripts/parity/review_diffs.py \
    --lang Swift --bucket real_rs,real_ref --limit 30
```

兩邊都收進來。real_rs 通常是「cgn 太敏感、額外 emit 了 ref 沒抓的東
西」— 多半是 EQUIV class 設定漏 / inclusive design。real_ref 才是
parser fix candidate。

### 2.4 EQUIV class 健康度檢查

```bash
python3 scripts/parity/review_diffs.py \
    --lang Kotlin --bucket label --limit 5
```

抽 5 個 label_diff 樣本，確認 `_EQUIV_CLASSES`（兩腳本同步）真有蓋到
這對 kind。若 packet 顯示「rs: Constructor / ref: Method」對應到同個
function declaration，而 EQUIV 已含 `{Method, Constructor, ...}`，就是
正常 label_diff，不用動。

---

## 3. Packet 範例（節錄）

設計重點：把源碼放在 entry 視覺中心，metadata 一行帶過、不喧賓奪主。

```markdown
### `ManuallyFailedException` @ src/Illuminate/Console/ManuallyFailedException.php:7

`real_rs` · cgn **Class** vs ref **—**

​```php
      1 │ <?php
      2 │ 
      3 │ namespace Illuminate\Console;
      4 │ 
      5 │ use RuntimeException;
      6 │ 
>>    7 │ class ManuallyFailedException extends RuntimeException
      8 │ {
      9 │     //
     10 │ }
​```

verdict: _____  (real_bug / label_diff / design / defensive)
```

各元素：

| 元素 | 用途 |
|---|---|
| `### `name` @ path:line` | 一行 locator — `vim +7 .sample_repo/PHP/src/.../ManuallyFailedException.php` 直接跳 |
| `bucket · cgn X vs ref Y` | diff 摘要一行打完（不一致才會出現在 packet，所以兩邊 kind 一定有差） |
| fenced code (±N lines) | **packet 的核心** — 實際源碼，`>>` 標 grep 命中行 |
| `verdict: _____` | 待填空格 |

整個 entry ~17 行，源碼占 11 行（context=±5 時）。reviewer 視線一路向
下：看 diff 摘要 → 掃 code → 寫 verdict。

---

## 4. 怎麼判 verdict

對照 README.md §1.4 規則表：

| 樣本性質 | verdict |
|---|---|
| function/method body 內的 transient declaration（local var、inner fn） | `design` |
| module-level 但被 arrow-fn 賦值的 const (`const X = () => {…}`) | `design`（cgn 故意 emit 成 Function 而非 Const） |
| type-level permanent 命名實體（struct field、enum variant field、impl method、class constant…） | `real_bug` |
| 同 declaration、兩邊 kind 不同但屬同 EQIUV class | `label_diff` |
| Test/Reference 路徑下被 builder filter 掉 | `design`（`is_non_production` filter） |
| query predicate guard，無實證 bug | `defensive` |

10 個樣本全 `design` / `label_diff` / `defensive` → 該 candidate 通過。
出現一個 `real_bug` → 中斷 packet 走訪、進入修補階段（query.scm + parser
分派 + regression test + reindex + 重 dump + 重 aggregate 確認該 kind
real_ref 歸零）。

---

## 5. 已知限制

### 5.1 「name 在檔內找不到」

ref-gitnexus 偶爾為 symbol 賦合成名（`anonymous_5`、tree-sitter
unnamed-node 推斷出來的識別字串），這類 name 不會在源碼裡出現。tool
寫 `> name X not found in <file>`，跳過。發生比例極低 (<1%)，發生時直
接視為 design / model 差，不挖。

### 5.2 多重 declaration 同名

`function foo()` overload、PHP class 內 `__construct` 多型 — grep 命中第
一個。reviewer 看到第一個若不對應 unpaired 那一條，目視掃同檔即可
（snippet 已給絕對行號）。

### 5.3 ext-only scoping 跨 lang 漏抓

`.h` files 同時被 C 和 Cpp lang dump 撈到（README.md §1.2 file-extension
scoping 副作用），review packet 會在 C lang 看到 `Cpp/foo.h` path。
腳本的 `resolve_file()` 已 fallback 從 `.sample_repo/<path>` 找，正常呈
現源碼，但 reviewer 應注意：那條 entry 屬 cross-lang spillover，verdict
通常為 `design`。

### 5.4 大規模 dump 檔案會很大

PHP real_rs 一個 lang 就 5048 筆，無 `--limit` cap 全 dump 大約 10 MB
markdown。實務上：

- 先用 `--limit 20` 抽 sample 走流程
- 確認該 kind 真要全 audit 才 `--limit 0`
- packet 是 derived artefact，跑完 audit 可直接 `rm scripts/parity/review/`

---

## 6. 跟其他工具的關係

```
dump_per_lang_symbols.py    →  symbol_diffs/<Lang>_*_all.txt
                                    │
                                    ├─→ parity_aggregate.py   (整體 gap 表，挑 candidate)
                                    │
                                    └─→ review_diffs.py       (per-symbol 人工 audit)
```

三支腳本共用同一份 `_all.txt`、同一份 `_EQUIV_CLASSES`（更動時三邊都
要同步）。review_diffs.py **完全是 read-only**，不會改 dump、不會碰
index、不會跑 cypher — 只是把現有資料重新排版成 reviewer-friendly 格
式。可以安全地反覆執行。

---

## 7. 常見 FAQ

**Q：每筆都要手動填 verdict 嗎？**
A：是的。real_bug vs design 的分流靠 enclosing context，無法純機械化判
定。tool 把 source context 端到面前就是為了讓人類眼睛 ~3 秒判一條。

**Q：packet 寫進 git 嗎？**
A：不寫。`scripts/parity/review/` 已在 `.gitignore` 補上（若尚未加，請
補一行）。它是 derived artefact，每次 reindex 後就 stale。Audit 完留下
的應該是 (a) parser fix + regression test commit，(b) README.md §4 案
例 trace，而不是 packet 本身。

**Q：和 `find_unpaired.py` 那種 `/tmp/` 腳本差別？**
A：`find_unpaired.py` 只列 `(path, name)` 文字 list，要人類自己 grep
+ 開檔。review_diffs.py 把 grep + 開檔 + 並列兩邊 emission 一次到位，
減 ~70% 手動操作。要看更深（grammar AST 結構等）時仍要落回 source
file / queries.scm。

**Q：為什麼我跑 `--bucket label` 數出來只有 aggregator label 的一半？**
A：aggregator 兩邊各算一次（label_diff 定義就是同 declaration 兩邊都
有 row，rs_only iterate 算一次、ref_only iterate 再算一次，所以一對
declaration 算 2）。review_diffs 第二輪會跳過已配對的 `(path, name)`，
所以一對 declaration 算 1。

兩支算的是不同問題：
- aggregator：「row-level disagreement 總量」 — 統計指標，雙算合理
- review_diffs：「要人工裁決的 unique declaration 數」 — packet 是給人
  類看的，同源碼 entry 重複出兩次只是重複工 + packet 膨脹

實際比例：PHP 一輪 aggregator label=1448、review_diffs label=724（剛好
1/2）。real_rs / real_ref / model 三類因為定義上不會跨邊重複，兩支數
字一致。

**Q：可以一次只看「不一致」的內容、把 EQUIV 配上的全濾掉嗎？**
A：那就是 default 行為。`--bucket real_rs,real_ref` 已排除 label
(EQUIV 配對) 與 model (一邊獨有 NodeKind)。要把 label 也加進來純粹是
為了 audit EQUIV class 設定，正常一輪驗證不用。
