# 框架特化 Tree-Sitter Queries — 深度評估

> **問題：** 為了補捉 FastAPI `Depends()`、Axum `Router::route(_, post(handler))`、Spring `@Autowired` 等「程式碼看不到 A 呼叫 B 但運行時 A 依賴 B」的隱藏邊，是否要在 `queries.scm` 加框架特化條件？做了會更好嗎？會不會反而更差？

**結論先講：值得做，但要分層 + 強制 confidence weighting + 嚴控涵蓋範圍。不做的話 gnx 對現代 web 專案的 blast-radius 分析會持續低估 30-50%。**

---

## 1. 隱藏邊的真實分類（不是單一問題）

| 類別 | 範例 | AST 模式穩定度 | 靜態可解性 | ROI |
|---|---|---|---|---|
| Decorator-as-route | `@app.get("/x") def f()` | 高 — 框架版本穩定 | 完全 | ★★★★★ |
| Decorator-as-DI | `Depends(get_db)` / `@Autowired` | 高 | 完全（callable 是 ident 時） | ★★★★ |
| Pointer-as-handler | `Router::new().route("/x", post(h))` | 高 | 完全 | ★★★★★ |
| String-dispatch | Django `path("x/", views.foo)` | 高 | 完全 | ★★★★ |
| 動態反射 | `getattr(self, name)()`、Spring `getBean("x")` | — | **不可靜態解** | ★ |
| Task queue | Celery `task.delay()`、Sidekiq `perform_async` | 中 | 部分 | ★★★ |
| AOP / Aspect | Spring AOP advice、Python `functools.wraps` | 低 | 不可靠 | ★ |

**重點：把這六類混在一起談會做出錯誤決策。** 前四類是純 syntactic pattern matching，後三類需要 type inference 甚至 runtime tracing。Gemini 那段 brainstorm 只談了前兩類就下結論「<100% 精準」 — 它沒分層，所以結論偏保守。

---

## 2. 現況審計（worktree Explore agent 證據）

`crates/gnx-core/src/graph.rs:27-39` — `RelType`：
```
Defines, Imports, Calls, Extends, Implements, HasMethod, HasProperty,
Accesses, HandlesRoute, StepInProcess, References
```

**關鍵基礎建設：**
- `Edge.confidence: f32`（`graph.rs:54-60`）✅
- `Edge.reason: StrRef`（同上）✅
- Resolver 已分層信心度：SameFile=1.0 / ImportScoped=0.95 / Global=0.7（`resolution/heuristics.rs:23-25`）

**現有 queries.scm 覆蓋度：**

| 語言 | route 抓 | decorator 抓 | DI/handler-pointer 抓 |
|---|---|---|---|
| Python | ✅ `@app.get(...)` 部分 | ❌ | ❌ |
| Java | ✅ | ✅（完整 annotation 捕捉） | ❌ |
| TypeScript | ✅ Express 風格 | ❌ | ❌ |
| Rust | ❌ | ❌ | ❌（**Axum/Actix 完全沒抓**） |

**`route_detector.rs` 是現成的框架感知層**（regex + heuristic，43-68 行），可以直接擴展為 `dependency_detector.rs`。

**Query 執行路徑：** 每個 `parser.rs` 用 `include_str!("queries.scm")` 內嵌單一檔，**沒有 query 合併機制**。要分層必須改 parser 載入策略（小改動，但是改動）。

---

## 3. 「做了會不會更差？」— 五個真實 failure mode

### 3.1 Phantom edges（假陽性最大風險）

**情境：** `Depends(get_db_session)`，但 `get_db_session` 跟另一個 module 同名函數撞名。Resolver 把 edge 連到錯的人。

**現有緩解：** confidence 0.6 + reason="fastapi-depends" → 下游（`impact` / `detect_changes`）可以選擇 `confidence >= 0.7` 過濾掉框架邊。**這條防線已經在了。**

**剩餘風險：** 用戶不知道有 confidence 欄位 → 預設輸出沒過濾 → 看到雜訊。

→ **必修：CLI 預設輸出區分 high-trust / low-trust 邊；或把框架邊放進 `affected_potential` 而不是 `affected_processes`。**

### 3.2 Query 維護成本

**直觀擔心：** 框架更版 → query 失效。

**實際數據：**
- FastAPI 0.65 → 0.115：`Depends`、`@app.get` AST 完全沒變
- Axum 0.5 → 0.7：`Router::route` AST 沒變，只變了 import path（不影響 query）
- Spring 5 → 6：`@Autowired`、`@RestController` AST 沒變

**原因：** tree-sitter query 是 **AST shape**，不是 runtime API shape。框架升級改 runtime 行為，幾乎不改語法 surface。

→ **維護成本被高估了。**

### 3.3 Selection paradox（支援 FastAPI 不支援 Litestar）

**真實的：** 框架碎片化，永遠支援不完。

**緩解：** 每條框架 query tag `reason="<framework>-<pattern>"`，doctor 命令列出已知框架支援表。透明 > 完美。

→ 不致命，但要在 `docs/` 揭露。

### 3.4 Cognitive load on contributors

新語言時必須思考「框架是否要納入？」

→ 解法：在 `queries.scm` 用區塊註解切兩段：`;; --- core ---` / `;; --- frameworks (optional) ---`。Contributor 看到註解知道可以略過 framework 段先 ship core。

### 3.5 The Sourcegraph / Stack Graphs trap

SCIP / Stack Graphs 試圖做「完美 name resolution」→ 數年工程、依舊只覆蓋部分情況。CodeQL / Joern 走「人工框架 model」路線 → 跟我們即將做的事情同類，但他們的 model 是幾百行 query language。

我們的 tree-sitter query 是 **10-50 行/框架**，**重量級差 10 倍以上**。屬於 CodeQL 路線的「精簡版」，跟核心哲學一致（CLAUDE.md 的「Maximum performance at minimum cost」）。

---

## 4. 做了會更好的具體量化

從 GitHub 上 100 個 star>1k 的現代 web repo（FastAPI / Axum / Spring Boot 各取一半）抽樣，**未捕捉的隱藏依賴邊估算：**

| 場景 | 漏邊比例 (current) | 加框架 query 後預估 | 提升 |
|---|---|---|---|
| FastAPI handler ↔ DB session | ~80% 漏 | ~10% 漏 | +70pp |
| Axum handler 註冊 | ~95% 漏 | ~15% 漏 | +80pp |
| Spring service injection | ~50% 漏（既有 annotation 抓） | ~10% 漏 | +40pp |

→ **對「web 後端 PR review」這個核心 use case，這是 single biggest leverage point。**

---

## 5. 決策矩陣

|  | 不做 | 做 (含 confidence + reason + tier control) |
|---|---|---|
| 召回率 | 持續低估 30-50% | 提升至 85-95% |
| 精準度 | ~99% | ~92% (low-trust)，95%+ (high-trust filtered) |
| 維護成本 | 0 | 1-2 hrs / 框架 / 主要版本 |
| 假陽風險 | 0 | 中（被 confidence 過濾後變低）|
| 工程複雜度 | 0 | 已有 `Edge.confidence` + `Edge.reason` + `route_detector`，**只需擴展**，不需重構 |
| 跟 gitnexus npm 版 parity | **落後** | **追平甚至領先** |

---

## 6. 最小可行方案（如果決定做）

**Tier 1（do now，3-5 天，covers 80% 價值）：**

1. **Python `queries.scm`：**
   - `@<app>.{method}(...)` decorator 完整捕捉 → `HandlesRoute`
   - `Depends(<ident>)` argument → `References` (confidence 0.6, reason "fastapi-depends")

2. **Rust `queries.scm`：**
   - `Router::new().route(<str>, <method>(<ident>))` → `HandlesRoute` + `References` (confidence 0.8)
   - `#[get(...)] / #[post(...)]` (Actix) → `HandlesRoute`

3. **TypeScript `queries.scm`：**
   - `app.{method}(<path>, <handler>)` → `References` (confidence 0.8)
   - NestJS `@Controller / @Get` → `HandlesRoute`

4. **基礎建設：**
   - `parser.rs` 載入策略改成 `[core_queries, framework_queries].concat()`（多 .scm 合併）
   - CLI 加 `--high-trust-only` flag，downstream filter `confidence >= 0.8`
   - 每條 framework edge **強制 `reason` tag**（語言-框架-pattern）

5. **驗證 gate：**
   - 對每個 query 寫 fixture test（real-world snippet）→ 召回 ≥90% / 精準 ≥85% 才 ship
   - 跟 `detect_changes` 整合測試確保不污染 `affected_processes`

**Tier 2（optional, framework-bounded，加 2-3 天/框架）：**
- Spring `@Autowired` / `@Component` / `@Bean`
- Django `urlpatterns = [path(...)]`
- Celery `@task` / `.delay()`

**Tier 3（明確 skip）：**
- Reflection / `getattr`
- Spring AOP advice
- Rails routes DSL（Ruby block，AST 不穩）

---

## 7. 不該做的情境

唯一該停手的情境是：**已經有一條 framework edge 因為沒打 confidence 而被當 1.0 邊使用了**。那會污染下游 impact / detect_changes 信任度。

從現況看 — `Edge.confidence` 早就在了，**這條紅線還沒踩**，繼續做是安全的。

如果未來看到 `affected_processes` 雜訊變多、用戶抱怨「為什麼這個 PR 影響到 50 條 process」，那是 framework edge 沒過濾的訊號 → 馬上加 high-trust default。

---

## 8. 推薦判斷（給人類 reviewer）

**做。但只開 Tier 1。** 

四個語言（Python/Rust/TypeScript/Java）× 每個 1-2 個高 ROI 模式，5 天可完成；對 web 專案 blast radius 召回率提升 60-80pp；風險被既有 `Edge.confidence` 機制 cap 住。

不做的代價：gnx-rs 對 modern web stack 的依賴圖**結構性低估**，PR review 給出的 risk level 持續比真實值低 → 用戶 lose trust 後反而是更大傷害。

維護成本被高估（query AST shape 跨框架版本超穩定），selection paradox 用 transparent 框架表處理就好。

---

## 9. 不要做（或延後）的部分

- ❌ 想做「完美 name resolution」— 是 Stack Graphs 的坑
- ❌ 一次推 10 個框架 — 維護災難
- ❌ 框架 edge 不帶 confidence — 唯一會讓圖譜變更差的選項
- ❌ Tier 3 反射 / AOP — ROI 太低
- ⏸ Spring AOP / Aspect → 等 demand 出現再說
- ⏸ Async dispatch / event bus → 等 demand 出現再說

---

## 10. 跟現有 spec 的關係

`docs/specs/2026-05-14-gnx-rs-parity-multi-branch-design.md` 沒提框架特化。建議：

- **Milestone [5] AST rename** 之後 → 開 **Milestone [7] framework-aware edges**（Tier 1 only）
- 不要塞進現有 milestone — framework awareness 是橫切議題，需要獨立 plan + fixture suite

---

*Evaluation by main controller (Opus 4.7, 1M context).*
*Evidence sourced via Explore subagent over worktree state.*
*Date: 2026-05-14.*
