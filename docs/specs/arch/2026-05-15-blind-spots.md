# Blind Spots — Truly Unresolvable Code Patterns

> **目標：** 對於**真不可靜態解**的程式碼模式（`eval` / `exec` / 動態 import / 跨物件反射 / `Class.forName` 等），不要沉默放過，**主動向 LLM 標記「這裡是我們看不到的盲區」**。
>
> 這是 LLM-first design 的概念封口：fan-out 處理「能列舉候選但選不出」，blind_spots 處理「連候選都列不出」。兩者組合 = 無 silent miss。

---

## 1. 什麼是 blind_spot

**Resolvable**（fan-out 處理）：靜態能列舉候選 universe
- `getattr(self, name)()` — 候選 = same-class methods
- `signal.connect(handler_instance)` — 候選 = handler class methods

**Blind spot**（這個 spec）：候選依賴 runtime data
- `eval(<string>)` / `exec(<string>)` — 完全動態
- `importlib.import_module(<var>)` — module 是 runtime 字串
- `__import__(<var>)` — 同上
- `getattr(<not-self>, name)()` — 跨物件反射，target class 未知
- `Class.forName(<string>)` (Java) — runtime class loading
- `Method.invoke(...)` (Java) — runtime method dispatch
- `new Function(<string>)` (JS/TS) — 動態 code eval
- `subprocess.run([cmd])` / `os.system(...)` — 外部 process

---

## 2. Schema

### 2.1 Type

```rust
// cgn-core/src/analyzer/types.rs
#[derive(Debug, Clone)]
pub struct BlindSpot {
    /// 偵測到的模式 kind，例如 "eval", "importlib-dynamic-import", "cross-object-getattr".
    pub kind: String,
    /// 觸發 blind_spot 的程式碼位置（在當前 file 內的 byte/row 範圍）.
    pub span: (u32, u32, u32, u32),
    /// LLM-readable hint，例如 "eval(arg) — runtime code execution, target unknown".
    pub hint: String,
}
```

### 2.2 LocalGraph 欄位

```rust
pub blind_spots: Vec<BlindSpot>,
```

---

## 3. 偵測模式（Phase 5 範圍）

**只做 Python（最常見的反射來源），其他語言留 Phase 5b。**

### 3.1 Python patterns

| Kind | Pattern | Tree-sitter capture |
|---|---|---|
| `python-eval` | `eval(...)` | `(call function: (identifier) @_f (#eq? @_f "eval"))` |
| `python-exec` | `exec(...)` | `(call function: (identifier) @_f (#eq? @_f "exec"))` |
| `python-compile` | `compile(...)` | `(call function: (identifier) @_f (#eq? @_f "compile"))` |
| `python-dynamic-import` | `importlib.import_module(<var>)` | `(call function: (attribute object: (identifier) @_m (#eq? @_m "importlib") attribute: (identifier) @_f (#eq? @_f "import_module")))` |
| `python-builtin-import` | `__import__(...)` | `(call function: (identifier) @_f (#eq? @_f "__import__"))` |
| `python-cross-getattr` | `getattr(<not-self>, name)()` | `(call function: (call function: (identifier) @_g (#eq? @_g "getattr") arguments: (argument_list . (identifier) @_obj (#not-eq? @_obj "self") . (identifier))))` |

### 3.2 Filter：static-arg 例外

如果 `eval` / `exec` / `import_module` 的參數是**字串字面值**（不是 variable），那其實是可解的 → 仍標 blind_spot 但 hint 提示「string literal — could be statically resolved」。

實作上：query 不 distinguish，parser.rs 在 emit 前 check capture 內第一 arg 是不是 `(string)` literal，是就跳過 blind_spot（不必標）或加 lower severity hint。**Phase 5 採後者**：標 blind_spot 但 hint 註明 string-literal-resolvable。

### 3.3 Hint 文案

LLM-friendly 句型：

```
"eval(<arg>) — runtime Python code execution; called function is not statically determinable"
"importlib.import_module(<arg>) — dynamic module loading; imported module name depends on runtime value"
"getattr(<obj>, name)() with obj != self — cross-object reflection; target class not enumerated by cgn Phase 2"
```

---

## 4. CLI 整合

### 4.1 `cgn admin index` summary

跑完 index 印出 footer：

```
Graph analysis complete.
  Scan time: ...
  ...
Blind spots detected: 7 across 3 files
  - dispatcher.py:42  cross-object-getattr (getattr(other, name)() ...)
  - config.py:18      python-dynamic-import (importlib.import_module(env_var))
  - plugin.py:9       python-eval (eval(user_input))
  ...
```

### 4.2 `cgn inspect X`

對 X 所在 file 的 blind_spots 加 `blind_spots[]` section in output：

```yaml
incoming: ...
outgoing: ...
blind_spots[2]{kind,line,hint}:
  python-eval,42,"eval(arg) — runtime Python code execution"
  python-dynamic-import,18,"importlib.import_module(...) — dynamic module loading"
```

### 4.3 `cgn impact --since HEAD~1`

加 `coverage.blind_spots_in_changed_files: N` 欄位，hint LLM「這些改動影響了 N 個 blind spot 站點，建議手動確認」。

**Phase 5 範圍只做 4.1（index summary） + 4.2（inspect）。** 4.3 留下一輪。

---

## 5. 儲存策略

`BlindSpot` 不是 edge — 不適合放 rkyv graph 主結構。兩個選項：

**Option A：放 graph 邊欄 metadata**
- `ZeroCopyGraph` 加 `pub blind_spots: Vec<BlindSpot>` (file-level)
- 查詢時掃這個欄位

**Option B：sidecar 檔**
- `<registry>/<repo>/<branch>/blind_spots.jsonl`
- analyze 時 write，CLI 命令 read

**Phase 5 採 Option A**：簡單、跟既有資料同 lifecycle、不增加檔案數。

`ZeroCopyGraph` 變更：加 `pub blind_spots: Vec<ZeroCopyBlindSpot>`，rkyv archive 處理。

---

## 6. 實作步驟

### Task A — types + builder pass through
- `BlindSpot` struct in `cgn-core/src/analyzer/types.rs`
- `LocalGraph.blind_spots`
- 修補所有 LocalGraph 建構點 `blind_spots: Vec::new()`
- `ZeroCopyGraph` 加 archived 對應
- Builder pass：把所有 local_graph.blind_spots 收集成 `graph.blind_spots`

### Task B — Python detector
- Append patterns to `python/frameworks.scm`
- python/parser.rs 收 captures，emit `BlindSpot` with kind/hint/span
- Test fixture + integration test

### Task C — CLI integration
- `analyze` command footer print blind_spots summary
- `context` command 加 `blind_spots[]` section
- 都讀 graph.blind_spots

---

## 7. 為什麼這是 LLM-first 設計的封口

**Tier 1+2：** 80% framework 覆蓋 — LLM 不知道剩下 20% 在哪
**fan-out (Phase 2)：** 反射可列舉部分 → confidence-weighted edges
**blind_spots (this)：** 反射不可列舉部分 → 明確標記位置 + kind + hint

LLM 拿到 cgn 回應後，邏輯變成：

```
1. 高信心 edges (>0.8) → 確定影響
2. 低信心 edges (<0.8) → fan-out 候選，sqrt 衰減提示
3. blind_spots → 我看不到的部分，需要 grep / 人工確認
4. 沒提到的 → 真的沒有
```

**無 silent miss。** 這個 contract 是 LLM-first design 的 holy grail。

---

## 8. 不在 Phase 5 範圍

- Java `Class.forName` / `Method.invoke` 偵測（Phase 5b）
- JS/TS `eval` / `new Function` 偵測（Phase 5b）
- `detect_changes --high-trust-only` 對 blind_spots 的整合
- `impact` 顯示 blind_spots
- Static-arg case 的特殊處理（這次都當 blind_spot）

---

*Spec by main controller (Opus 4.7, 1M context). Date: 2026-05-15.*
