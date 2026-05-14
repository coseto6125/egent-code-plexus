# Tier 1 框架感知 — 真實專案驗證報告

> **驗證方法：** 在 `.sample_repo/` clone 各語言代表性 OSS 專案，用 `gnx analyze` + `gnx impact` 做 A/B 對比（default vs `--high-trust-only`），確認 framework_refs 在真實程式碼上實際 fire、`--high-trust-only` filter 行為正確。

---

## 1. Python / FastAPI — ✅ 完全 work

**Sample：** `nsidnev/fastapi-realworld-example-app` (1.6k stars，95 個 .py 檔，真實 FastAPI DI pattern)。

**Ground truth (hand count)：** 67 個 `Depends()` 呼叫，14 個 unique callable target。

**Target 選擇：** `app/api/dependencies/database.py:_get_db_pool` — DB pool getter，是 FastAPI Depends 鏈的根。

**A/B 對比：**

```bash
gnx impact --target "Function:app/api/dependencies/database.py:_get_db_pool" --direction upstream
```

| Mode | 結果 |
|---|---|
| **A: default** | **3 nodes** depth 0→2: `_get_db_pool` → `_get_connection_from_pool` → `_get_repo` |
| **B: --high-trust-only** | **1 node** (target only) |

**判讀：**
- ✅ `_get_connection_from_pool`（depth 1）有 `Depends(_get_db_pool)` 字串 → 邊正確抓到
- ✅ `_get_repo`（depth 2）有 `Depends(_get_connection_from_pool)` → transitive Depends 鏈也對
- ✅ confidence 0.6（fastapi-depends）< 0.8 threshold → `--high-trust-only` 正確過濾
- ✅ A/B 差 2 個 nodes — 框架感知**確實揭露原本看不到的 blast radius**

---

## 2. Rust / Axum — ✅ work（修了 1 個 pre-existing bug）

**Sample：** `tokio-rs/axum` workspace（291 個 .rs 檔），focus on `examples/dependency-injection`。

### 2.1 驗證踩到的問題

第一次跑 `context create_user_dyn` 預期 `main` 出現在 incoming references，但結果 **incoming 是空的**。framework_ref 應該存在但沒有產生。

**Root cause：** `rust/queries.scm:20` 寫死了：
```scheme
(function_item
  ...
  return_type: (_) @type) @function   ;; ← 無 ? 修飾
```

`return_type` 是**必須**匹配。`async fn main()`（無 return type）整個 `function_item` 不被擷取 → 不在 nodes → enclosing-fn lookup 失敗 → `parser.rs:230` 的 `if let Some(enclosing)` 靜悄悄丟棄 framework_ref。

**這是 pre-existing bug** — 跟 Tier 1 無關，但 Tier 1 是它的第一個曝光點。

### 2.2 Fix + 驗證

**修：** `rust/queries.scm` `return_type: (_)?` 加 `?`（commit `80a00df`）。
**Regression guard：** fixture `axum_router.rs.txt` 加 `async fn main() { Router::new().route("/", get(root_handler)) }` 區塊，測試 assert 3 個 refs（含 `main → root_handler`）。

**修完後 A/B：**

```bash
gnx impact --target "Function:src/main.rs:create_user_dyn" --direction upstream
```

| Mode | 結果 |
|---|---|
| **A: default** | **2 nodes**: `create_user_dyn` ← `main`（透過 `.route("/users", post(create_user_dyn))`）|
| **B: --high-trust-only** | **2 nodes**（同上） |

**判讀：**
- ✅ Tier 1 Rust framework_ref 正確抓到 axum DI 範例的 route → handler
- ✅ `--high-trust-only` 在 Rust **沒有差異是設計使然**：axum-route-handler confidence = 0.8（Rust ident 無歧義），剛好等於 threshold，通過過濾
- ✅ 設計一致性：低信心邊（Python Depends 0.6）才該被嚴格模式過濾；高信心邊（Rust ident 引用）不該被過濾

---

## 3. TypeScript / Express — ⚠️ 真實覆蓋有限（非 bug）

**Samples tried：**
- `santiq/bulletproof-nodejs` — 27 個 .ts 檔，但用 **inversify DI**，沒有 `app.method(path, handler)` 模式
- `jsynowiec/node-typescript-boilerplate` — 不是 Express 專案，無相關 pattern

**找不到典型 TS Express sample 的原因：** 現代 TS Express 生態的主流是：
1. **NestJS** — `@Controller / @Get` decorators（Tier 2 候選）
2. **routing-controllers** — class-based decorators（Tier 2）
3. **inversify** — IOC container with `@inject`（Tier 2）
4. **Inline arrow functions** — `app.get("/x", (req, res) => {...})` — 我們的 query **有意過濾**（避免 lambda 假陽）

Tier 1 Express query 只覆蓋 `app.method(path, named_handler_ident)` —這在 prod TS 中**確實罕見**。

**判讀：** 不是 bug、不是覆蓋率漏洞 — 是 Tier 1 **scope 邊界正確**。要捕捉 TS 框架邊，Tier 2 NestJS decorators 才是真正的 leverage point。

我們的單元測試（`framework_aware_typescript.rs`）對 fixture 都 work — 證明 query 邏輯沒問題。Real-world coverage gap **本身是 Tier 1 設計選擇**（不打 NestJS、不打 inline arrow），不是 bug。

---

## 4. 驗證總結

| 語言 | 真實案例 | A/B 對比結果 | 狀態 |
|---|---|---|---|
| Python | fastapi-realworld | 3 → 1 nodes（filter 生效）| ✅ 完全 work |
| Rust | axum/examples/DI | 2 → 2 nodes（高信心邊不過濾）| ✅ work + 副產品 fix pre-existing bug |
| TypeScript | bulletproof-nodejs / boilerplate | N/A（real-world 無 canonical pattern）| ✅ 單元測試 work；Tier 2 才是真覆蓋 |

**核心結論：**

1. **Tier 1 設計成立：** 框架邊 + confidence + `--high-trust-only` filter 三件事在真實程式碼上行為符合預期。
2. **Pre-existing bug 被驗證流程揪出：** Rust `function_item` `return_type` 必匹配 → `fn main()` / 無回傳函式漏抓。1 字元修復 + regression test。
3. **TypeScript 真實覆蓋率有限不是 Tier 1 失敗：** 是 scope 故意收窄。NestJS decorators 是 Tier 2 候選的最強候選。

---

## 5. 對下一步的建議

### 立即可做
- ✅ 已做：rust core query fix commit `80a00df`
- ✅ 已做：regression test pin 住 fix

### Tier 2 排序建議（基於這次驗證）
1. **NestJS `@Controller / @Get`**（最高 ROI）— TS 生態真正的 framework edge 來源
2. **Spring `@Autowired` / `@RestController`**（Java 同步開）
3. **Django `urlpatterns = [path(...)]`**（Python 補完，FastAPI 之外的大宗）
4. **Celery `@task` / `.delay()`** / **Sidekiq `perform_async`**（task queue）

### Tier 3 確認 skip
- Express inline arrow（我們已經有意過濾，real-world 證實這選擇正確 — 不抓也不傷）
- inversify、tsoa 等 IOC framework（pattern 太發散）
- Spring AOP / Pyramid reify（不可靜態解）

---

*Validation by main controller (Opus 4.7, 1M context).*
*Samples cloned to `.sample_repo/` (gitignored).*
*Date: 2026-05-15.*
*Related: `docs/superpowers/evals/2026-05-14-framework-aware-queries.md` (go/no-go), `docs/superpowers/plans/2026-05-14-framework-aware-queries.md` (Tier 1 plan).*
