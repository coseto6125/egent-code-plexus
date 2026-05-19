# Framework-Aware Tree-Sitter Queries (Tier 1) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** 在 code-graph-nexus 加入框架感知 edges（FastAPI Depends、Axum Router::route 等），讓 web 後端依賴圖召回率從 ~50% 提升至 ~90%，同時用 `Edge.confidence` 限制誤判風險。

**Architecture:** 沿用既有 Raw* → builder → Edge 的 pipeline。新增 `RawFrameworkRef` 型別（含 confidence + reason tag），各語言 `parser.rs` 用「core queries + framework queries」雙 .scm 合併載入 → 抽出 → builder 解析為 Edge with `confidence < 1.0`。CLI 加 `--high-trust-only` flag 讓下游選擇過濾。

**Tech Stack:** tree-sitter 0.25 query syntax, Rust 2021, 既有 cgn-core/cgn-analyzer crate boundary。

**Eval reference:** `docs/evals/2026-05-14-framework-aware-queries.md`。

---

## File Structure

**Create:**
- `crates/cgn-analyzer/src/python/frameworks.scm` — Python 框架 query（FastAPI / Flask 子集）
- `crates/cgn-analyzer/src/rust/frameworks.scm` — Rust 框架 query（Axum / Actix）
- `crates/cgn-analyzer/src/typescript/frameworks.scm` — TS 框架 query（Express / NestJS）
- `crates/cgn-cli/tests/fixtures/fastapi_depends.py` — fixture
- `crates/cgn-cli/tests/fixtures/axum_router.rs` — fixture
- `crates/cgn-cli/tests/fixtures/express_app.ts` — fixture
- `crates/cgn-cli/tests/framework_aware.rs` — 整合測試

**Modify:**
- `crates/cgn-core/src/analyzer/types.rs` — 加 `RawFrameworkRef` + `LocalGraph.framework_refs`
- `crates/cgn-analyzer/src/python/parser.rs` — 合併 .scm + 處理 framework captures
- `crates/cgn-analyzer/src/rust/parser.rs` — 同上
- `crates/cgn-analyzer/src/typescript/parser.rs` — 同上
- `crates/cgn-analyzer/src/resolution/builder.rs` — Pass 2 解析 `framework_refs` → `Edge`
- `crates/cgn-cli/src/commands/detect_changes.rs` — 加 `--high-trust-only` flag

---

### Task 1: Infra — `RawFrameworkRef` 型別 + builder pass

**Files:**
- Modify: `crates/cgn-core/src/analyzer/types.rs`
- Modify: `crates/cgn-analyzer/src/resolution/builder.rs`
- Test: `crates/cgn-analyzer/src/resolution/builder.rs` (inline `#[cfg(test)]`)

**Steps:**

- [ ] **Step 1: Write failing test** in `builder.rs` 末尾的 `#[cfg(test)] mod tests`：

```rust
#[test]
fn framework_ref_produces_edge_with_confidence_and_reason() {
    use crate::analyzer::types::{LocalGraph, RawFrameworkRef, RawNode};
    use cgn_core::graph::NodeKind;
    
    let mut g = LocalGraph {
        file_path: "test.py".into(),
        content_hash: [0; 32],
        nodes: vec![
            RawNode { name: "handler".into(), kind: NodeKind::Function, span: (0,0,0,0), is_exported: false, heritage: vec![], type_annotation: None, decorators: vec![], calls: vec![] },
            RawNode { name: "get_db".into(), kind: NodeKind::Function, span: (0,0,0,0), is_exported: false, heritage: vec![], type_annotation: None, decorators: vec![], calls: vec![] },
        ],
        documents: vec![],
        imports: vec![],
        routes: vec![],
        framework_refs: vec![RawFrameworkRef {
            source_name: "handler".into(),
            target_name: "get_db".into(),
            confidence: 0.6,
            reason: "fastapi-depends".into(),
            span: (0,0,0,0),
        }],
    };
    
    let graph = GraphBuilder::new().add_local(g).build();
    let fw_edges: Vec<_> = graph.edges().iter()
        .filter(|e| e.rel_type == RelType::References)
        .collect();
    assert_eq!(fw_edges.len(), 1);
    assert!((fw_edges[0].confidence - 0.6).abs() < 1e-6);
    assert_eq!(fw_edges[0].reason.as_str(), "fastapi-depends");
}
```

- [ ] **Step 2: Run** `cargo test -p cgn-analyzer framework_ref_produces_edge` — expect compile FAIL（`RawFrameworkRef` 不存在）。

- [ ] **Step 3: Add `RawFrameworkRef` type** to `crates/cgn-core/src/analyzer/types.rs`：

```rust
#[derive(Debug, Clone)]
pub struct RawFrameworkRef {
    pub source_name: String,
    pub target_name: String,
    pub confidence: f32,
    pub reason: String,
    pub span: (u32, u32, u32, u32),
}
```

Add field to `LocalGraph`:

```rust
pub framework_refs: Vec<RawFrameworkRef>,
```

- [ ] **Step 4: Update all `LocalGraph` construction sites** to include `framework_refs: Vec::new()`. Use grep `LocalGraph {` across `crates/cgn-analyzer/src/*/parser.rs`. Should be ~20 files; default empty.

- [ ] **Step 5: Add builder pass** in `builder.rs` build()，緊接 routes processing 之後，掃 `local.framework_refs`，target_name 走既有 same-file / import scoped 解析，emit `Edge { src, dst, rel_type: RelType::References, confidence: ref.confidence, reason: intern(&ref.reason) }`。

- [ ] **Step 6: Run** test — expect PASS。Run 整個 `cargo test -p cgn-analyzer` 確認沒退化。

- [ ] **Step 7: Commit**

```bash
git add crates/cgn-core/src/analyzer/types.rs crates/cgn-analyzer/src/resolution/builder.rs crates/cgn-analyzer/src/*/parser.rs
git commit -m "feat(core): add RawFrameworkRef type for confidence-weighted framework edges"
```

---

### Task 2: Python — FastAPI `Depends()` + `@app.<method>()`

**Files:**
- Create: `crates/cgn-analyzer/src/python/frameworks.scm`
- Modify: `crates/cgn-analyzer/src/python/parser.rs`
- Create: `crates/cgn-cli/tests/fixtures/fastapi_depends.py`
- Create or extend test in `crates/cgn-cli/tests/framework_aware.rs`

**Depends on:** Task 1.

**Steps:**

- [ ] **Step 1: Fixture file** `fastapi_depends.py`:

```python
from fastapi import FastAPI, Depends

app = FastAPI()

def get_db():
    return None

def get_current_user(db = Depends(get_db)):
    return None

@app.get("/users/{id}")
def read_user(id: int, user = Depends(get_current_user)):
    return user
```

- [ ] **Step 2: Failing integration test** in `framework_aware.rs`:

```rust
#[test]
fn fastapi_depends_creates_low_confidence_reference() {
    let src = include_str!("fixtures/fastapi_depends.py");
    let provider = cgn_analyzer::python::PythonProvider::new().unwrap();
    let local = provider.parse_file("test.py".as_ref(), src.as_bytes()).unwrap();
    
    // Expect 2 framework_refs:
    // get_current_user --Depends(get_db)--> get_db
    // read_user --Depends(get_current_user)--> get_current_user
    assert_eq!(local.framework_refs.len(), 2);
    let pairs: Vec<(&str, &str)> = local.framework_refs.iter()
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(pairs.contains(&("get_current_user", "get_db")));
    assert!(pairs.contains(&("read_user", "get_current_user")));
    for r in &local.framework_refs {
        assert!(r.confidence < 1.0 && r.confidence > 0.0);
        assert!(r.reason.starts_with("fastapi-"));
    }
}
```

- [ ] **Step 3: Run** — expect FAIL（framework_refs 是空的）。

- [ ] **Step 4: Create `frameworks.scm`**：

```scheme
;; FastAPI: Depends(<callable>) — captures the callable identifier.
;; Emitted as RawFrameworkRef from the enclosing function.
(call
  function: (identifier) @_fn (#eq? @_fn "Depends")
  arguments: (argument_list
    (identifier) @fastapi.depends.target)) @fastapi.depends.call

;; FastAPI: @<app>.{get,post,put,delete,patch}("/path")
;; Already partly captured by core; framework rule disambiguates.
(decorator
  (call
    function: (attribute
      object: (identifier) @_app
      attribute: (identifier) @fastapi.route.method)
    arguments: (argument_list
      (string (string_content) @fastapi.route.path))))
```

- [ ] **Step 5: Update `python/parser.rs`** to merge queries at load:

```rust
let query_source = format!(
    "{}\n;; ---- framework queries ----\n{}",
    include_str!("queries.scm"),
    include_str!("frameworks.scm"),
);
let query = Query::new(&language, &query_source)?;
```

- [ ] **Step 6: Add capture handlers** in `parse_file()` 處理 `fastapi.depends.target` capture：找到 enclosing function（用 span containment 找 nodes 中包含此 capture span 的 fn/method），emit `RawFrameworkRef { source_name: enclosing.name, target_name: capture.text, confidence: 0.6, reason: "fastapi-depends".into(), span }`。

- [ ] **Step 7: Run** test — expect PASS。

- [ ] **Step 8: Commit**

```bash
git add crates/cgn-analyzer/src/python/frameworks.scm crates/cgn-analyzer/src/python/parser.rs crates/cgn-cli/tests/fixtures/fastapi_depends.py crates/cgn-cli/tests/framework_aware.rs
git commit -m "feat(python): emit framework refs for FastAPI Depends() and route decorators"
```

---

### Task 3: Rust — Axum `Router::route` + Actix `#[get]`

**Files:**
- Create: `crates/cgn-analyzer/src/rust/frameworks.scm`
- Modify: `crates/cgn-analyzer/src/rust/parser.rs`
- Create: `crates/cgn-cli/tests/fixtures/axum_router.rs.txt` (avoid Cargo picking it up — `.txt` extension; `include_str!` 處用相對路徑)
- Extend: `crates/cgn-cli/tests/framework_aware.rs`

**Depends on:** Task 1.

**Steps:** 同 Task 2 模式。

- [ ] **Step 1: Fixture**：

```rust
// 存成 axum_router.rs.txt 避免被 cargo build
use axum::{Router, routing::{get, post}};

async fn login_handler() -> &'static str { "ok" }
async fn logout_handler() -> &'static str { "bye" }

fn build_routes() -> Router {
    Router::new()
        .route("/login", post(login_handler))
        .route("/logout", get(logout_handler))
}
```

- [ ] **Step 2: Failing test** asserting 2 framework_refs (`build_routes` → `login_handler`, `build_routes` → `logout_handler`)，每條 reason="axum-route-handler", confidence=0.8。

- [ ] **Step 3: `frameworks.scm`** — match `.route(<string_lit>, (get|post|put|delete|patch)(<ident>))`：

```scheme
;; Axum: .route("/path", METHOD(handler_ident))
(call_expression
  function: (field_expression
    field: (field_identifier) @_route (#eq? @_route "route"))
  arguments: (arguments
    (string_literal) @axum.route.path
    (call_expression
      function: (identifier) @axum.route.method
      arguments: (arguments
        (identifier) @axum.route.handler))))
```

- [ ] **Step 4-6:** Merge .scm in parser.rs, add capture handler that resolves enclosing fn for span, emit RawFrameworkRef (confidence 0.8 — Rust handler ident is unambiguous).

- [ ] **Step 7: Commit**

```bash
git commit -m "feat(rust): emit framework refs for Axum Router::route handlers"
```

---

### Task 4: TypeScript — Express `app.<method>` + NestJS `@Controller/@Get`

**Files:**
- Create: `crates/cgn-analyzer/src/typescript/frameworks.scm`
- Modify: `crates/cgn-analyzer/src/typescript/parser.rs`
- Create: `crates/cgn-cli/tests/fixtures/express_app.ts`
- Extend: `crates/cgn-cli/tests/framework_aware.rs`

**Depends on:** Task 1.

**Steps:** 同 Task 2 模式。 

- [ ] **Step 1: Fixture (Express form)**:

```typescript
import express from "express";

const app = express();

function loginHandler(req: any, res: any) { res.send("ok"); }
function logoutHandler(req: any, res: any) { res.send("bye"); }

app.get("/login", loginHandler);
app.post("/logout", logoutHandler);
```

- [ ] **Step 2: Failing test** — top-level `loginHandler` / `logoutHandler` 各一條 framework_ref (source_name 為 module-level 偽 source `"<module>"` 或 `""`，這個細節 implementer 自決，但要在 test 鎖死)，reason="express-route-handler"。

- [ ] **Step 3-6:** `frameworks.scm` match `<id>.{get,post,put,delete,patch,use}(<string>, <ident>)`，capture method + path + handler ident。

- [ ] **Step 7: Commit**

```bash
git commit -m "feat(typescript): emit framework refs for Express route handlers"
```

---

### Task 5: CLI — `--high-trust-only` flag + integration test

**Files:**
- Modify: `crates/cgn-cli/src/commands/detect_changes.rs`
- Modify: `crates/cgn-cli/src/commands/impact.rs`
- Extend: `crates/cgn-cli/tests/framework_aware.rs`

**Depends on:** Task 2 (至少一個 lang 有 framework edges，才測得到 filter)。

**Steps:**

- [ ] **Step 1: Failing integration test**：跑 detect-changes on FastAPI fixture，`--high-trust-only` 開時 affected_count 較低（不含 fastapi-depends 邊的下游），關時較高。

- [ ] **Step 2: Add `--high-trust-only` flag** (default `false`) 到 detect_changes CLI subcommand 與 impact CLI subcommand。

- [ ] **Step 3: Wire flag** 到圖遍歷 — 當 flag=true，traversal 排除 `edge.confidence < 0.8`。

- [ ] **Step 4: Run test** — expect PASS。

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(cli): add --high-trust-only flag to filter low-confidence framework edges"
```

---

## Execution Strategy

**Sequential:** Task 1（infra）必須先完成 — 其它都依賴 `RawFrameworkRef`。

**Parallel:** Task 2、3、4 完全獨立 — 可派三個併發 implementer。

**Sequential:** Task 5 最後 — 需要至少一個 lang 完成才能整合測試。

**Total estimate:** 5 days serial / 3 days with parallel T2-4.

---

## Self-Review Checklist

- [ ] 每條 framework edge 都有 `confidence < 1.0` ✅
- [ ] 每條 framework edge 都有 `reason` tag（"fastapi-depends" / "axum-route-handler" / "express-route-handler"）✅
- [ ] CLI `--high-trust-only` 預設關（向後相容）✅
- [ ] 既有測試（detect_changes、analyze）不退化 — Task 1 完成後跑 `cargo test --workspace`
- [ ] Fixture 涵蓋 happy path；不追求 framework variant 全覆蓋（後續 milestone 再說）
- [ ] 不觸碰反射 / AOP / 動態 dispatch — Tier 3 明確 skip

---

## Out of Scope (明確 NOT 做)

- Spring `@Autowired` / `@Bean`（Tier 2，後續開）
- Django `urlpatterns` / Celery `@task`（Tier 2）
- Flask 全功能（Tier 2 — 跟 FastAPI 不同）
- Reflection `getattr` / `BeanFactory.getBean()` (Tier 3 skip)
- Spring AOP advice (Tier 3 skip)
- Cross-file resolution 強化 — 用既有 import-scoped resolver 就好
