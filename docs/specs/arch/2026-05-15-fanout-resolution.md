# Fan-out Resolution — LLM-First Static Analysis Spec

> **核心定位：** graph-nexus 主要面向是 LLM 消費者。LLM 對「沈默漏網」高度敏感 — 一次 query 拿到的資訊，就是它推理的全部依據。所以 gnx 的設計原則不是「高信心邊優先」，而是 **「完整 universe + confidence 編碼不確定性」**，讓 LLM 一次查完，自己根據 confidence 加權判斷。

---

## 1. 設計哲學

**舊模式（Tier 1+2）：**
- 一個 call site → 一條邊（or 跳過）
- 不確定 → 跳過
- LLM 看到 0 edges = 真的沒人呼叫
- **問題：** silent miss → LLM 幻覺

**Fan-out 模式：**
- 一個 ambiguous call site → **N 條候選邊**，每條 confidence = `base / N`
- 不確定 → emit 全候選 + 低 confidence
- LLM 看到 N edges with conf 0.15 each = 「不確定，但 universe 是這些」
- **效果：** 0 silent miss（除非真不可解，那個進 `blind_spots`）

---

## 2. Phase 範圍

| Phase | 模式 | 範圍 |
|---|---|---|
| **Phase 2 (this spec)** | `getattr(obj, name)()` 反射呼叫 | Python only，先 same-file class methods |
| Phase 1 | YAML/JSON config routes | 後續 |
| Phase 3 | Dynamic `signal.connect(complex)` | 後續 |
| Phase 4 | Spring AOP simple pointcut | 後續 |
| Phase 5 | 真 `blind_spots` (eval/importlib/IPC) | 後續 |

Phase 2 先建立「fan-out」的 architecture，後續 phases 沿用同 model。

---

## 3. Phase 2 詳細設計

### 3.1 偵測模式（Python）

**Pattern A — `getattr(self, name)()` 同類別 dynamic dispatch：**

```python
class Dispatcher:
    def dispatch(self, action: str, data):
        method_name = f"handle_{action}"
        return getattr(self, method_name)(data)   # ← 抓這個
    
    def handle_create(self, data): ...
    def handle_delete(self, data): ...
    def handle_update(self, data): ...
```

→ emit 3 fan-out edges：`dispatch` → `handle_create` / `handle_delete` / `handle_update`

confidence = 1.0 / 3 ≈ 0.33 (base 1.0, N=3 candidates)

**Pattern B — `getattr(self, name, default)()` 帶 default：**

```python
return getattr(self, f"handle_{action}", self.handle_unknown)()
```

→ 同 Pattern A，加 `handle_unknown` 進 candidates

**Pattern C — `getattr(cls, name)()` 同 class 但用 class ref：**

```python
return getattr(MyClass, name)(instance)
```

→ 同 Pattern A，但 source 改成 module 層級

### 3.2 不在 Phase 2 範圍

- `getattr(other_object, name)()` 跨類別 → Phase 後續做
- `getattr` 但結果存 variable 再 call → Phase 後續
- `obj[name]()` 用 `__getitem__` → 設計類似但不同
- `eval(code_string)` → 真 blind_spot
- `importlib.import_module(name)` → 真 blind_spot

### 3.3 Resolution 規則

對 `getattr(self, X)()`：
1. 找 enclosing method 的 enclosing class（同檔內）
2. enumerate class 內所有 `def method_name(self, ...)` (NodeKind::Method, kind==Method)
3. **filter heuristic**：
   - 過濾掉 dunder methods (`__init__`, `__repr__`, etc.) — 不太可能被 dynamic dispatch
   - 過濾掉 caller 自己（避免遞迴 fanout）
4. emit edges：source = caller method UID，target = 各 candidate method UID

對 `getattr(SomeClass, X)()`：
1. resolver 找 SomeClass 在當前 file scope 內定義
2. 同上 enumerate 該 class 的 methods
3. emit edges：source = caller，target = 各 method

### 3.4 Confidence 公式

```
base_confidence = 0.5  // 反射本質就低信心
N = 候選方法數量
confidence_per_edge = base_confidence / sqrt(N)  // 平方根衰減，避免 N 太大時 conf 0
最低 cap = 0.1   // 不低於這個
```

例：
- 3 candidates → 0.5 / √3 ≈ 0.29
- 10 candidates → 0.5 / √10 ≈ 0.16
- 50 candidates → 0.1 (cap)

`reason` = `"reflection-getattr-fanout"`（讓 LLM 看到就知道是 fan-out）

`--high-trust-only` (≥ 0.8) 會把整批 fan-out 過濾掉 — 設計使然，嚴格模式下不要這類邊。

### 3.5 Schema 變更

**Option 1：複用既有 `RawFrameworkRef`**

優點：少改 type system。
缺點：multi-target 變成 emit N 條 ref，relation 上看不出來是 fan-out。

**Option 2：新增 `RawFanoutRef` 結構**

```rust
#[derive(Debug, Clone)]
pub struct RawFanoutRef {
    pub source_name: String,
    pub candidates: Vec<String>,        // 候選 target names
    pub base_confidence: f32,            // 0.5
    pub reason: String,                  // "reflection-getattr-fanout"
    pub span: (u32, u32, u32, u32),
}
```

`LocalGraph` 加：
```rust
pub fanout_refs: Vec<RawFanoutRef>,
```

builder 處理時 resolve candidates → 對每個 resolved target emit Edge with `confidence = base / sqrt(N)`。

**推薦 Option 2** — schema 顯式表達 fan-out 概念，方便未來查詢「找出所有 fan-out 來源」。

### 3.6 LLM-visible 輸出

`gnx inspect dispatch` 應該回：

```yaml
outgoing:
  references[3]{filePath,name,uid,confidence,reason}:
    src/dispatcher.py,handle_create,Function:src/dispatcher.py:handle_create,0.29,reflection-getattr-fanout
    src/dispatcher.py,handle_delete,Function:src/dispatcher.py:handle_delete,0.29,reflection-getattr-fanout
    src/dispatcher.py,handle_update,Function:src/dispatcher.py:handle_update,0.29,reflection-getattr-fanout
```

LLM 看到 3 條 0.29 conf 的 `reflection-getattr-fanout` 邊 →
- 知道 dispatch 內部用了 dynamic dispatch
- 知道候選 universe 是這三個 handle_*
- 知道每條都 0.29 信心（其中一條才是真實 call path）
- 不需要再 grep，已有完整資訊

---

## 4. 實作步驟

### Task 1 — types & infra
- Create `RawFanoutRef` in `graph-nexus-core/src/analyzer/types.rs`
- Add `pub fanout_refs: Vec<RawFanoutRef>` 到 `LocalGraph`
- 修補所有 LocalGraph 建構點（~30 處 parser.rs）`fanout_refs: Vec::new()`
- Builder Pass 新增 fanout_refs 處理：resolve candidates, emit Edge per candidate with `confidence = base / sqrt(N).max(0.1)`
- Test：unit test 在 builder.rs 驗 3 candidates 產 3 edges with conf ≈ 0.29

### Task 2 — Python `getattr(self, name)()` detector
- Append to `python/frameworks.scm`：抓 `getattr(self, ...)()` call site
- python/parser.rs：
  - 取 capture 對應的 byte range
  - 找 enclosing method（用 `framework_helpers::enclosing_function_name`）
  - 找 enclosing class（looking up parent class node — 需新 helper）
  - enumerate same-class methods（過濾 dunder + self）
  - emit `RawFanoutRef { source_name=enclosing_method, candidates=[methods], base=0.5, reason="reflection-getattr-fanout" }`
- Integration test：fixture 內 dispatcher class + 3 handlers，assert 3 edges with conf in [0.25, 0.35]

### Task 3 — Edge 案例 + negative tests
- `getattr(self, "fixed_name")()` 字串字面值（不是 fan-out — 該抓單一邊 with high conf）
- `getattr(other_obj, name)()` 跨物件（Phase 2 範圍外，必須跳過）
- `obj.method()` 普通呼叫（不該被誤抓）

### Task 4 — docs
- 更新 `docs/evals/` 加 Phase 2 設計記錄
- Update README（如有）說明 fanout edges 的解讀

---

## 5. 風險與緩解

| 風險 | 緩解 |
|---|---|
| 一個檔案內 100 個 methods → fan-out 邊爆炸 | confidence 平方根衰減 + cap 0.1；下游 `--high-trust-only` 過濾 |
| Method enumerate 找錯 class（inheritance） | Phase 2 限同檔內，不追 inheritance；明確 doc 限制 |
| 字串字面值誤判 fan-out | query 加 predicate：`name` 必須是 `(identifier)` 或 `(call)` / `(f-string)`，不可是 `(string)` literal |
| LLM 不懂 fan-out 概念 | `reason = reflection-getattr-fanout` 自我說明 |
| Builder pass 複雜度 | 純線性掃 fanout_refs，每條 O(M) resolve（M = candidates），無 N² |

---

## 6. 成功驗證

**Real-world test：clone 一個有 dispatcher pattern 的 OSS Python 專案**
- e.g. `django/django` 內有不少 `getattr(self, "_meta_X")` 模式
- 或 `home-assistant/core` 內有大量 service handler dispatch
- 跑 `gnx admin index` 後 `gnx inspect` 看 dispatch method 是否出現 fan-out edges
- 用 `--high-trust-only` 確認嚴格模式排除這些

---

## 7. 不在 Phase 2 範圍（明確 defer）

- JS/TS `obj[name]()` 動態方法呼叫
- Java `Class.forName().newInstance()`
- 跨檔 class 追蹤
- 繼承鏈 method enumerate
- `eval()` / `exec()` / `importlib.import_module()`
- Plugin entry_points
- Spring AOP
- 大部分 Tier 3 名單上的東西（後續 Phase 處理或進 blind_spots）

---

*Spec by main controller (Opus 4.7, 1M context). Date: 2026-05-15.*
