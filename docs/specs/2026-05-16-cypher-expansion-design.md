# Cypher Expansion Design

**Date**: 2026-05-16
**Owner**: cypher subsystem
**Status**: spec approved, plan pending

## 動機

現行 `cgn cypher` 只支援單一 regex 解析的最小 pattern：

```
MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE a.name='val'] RETURN a,b
```

對 LLM agent 在 graph 上做診斷查詢嚴重不夠用——無法表達多 hop chain、無法做 edge property filter、無 LIMIT/ORDER、無 aggregation、無 OPTIONAL MATCH、無 label alternation。本設計把 cypher 升級為「合理的 openCypher read-only 子集」，並改寫輸出為 column-based 形狀（LLM 與 jq 都友善），同時支援 TOON 序列化以節省 LLM 上下文 token。

## 範圍

### 支援

```cypher
MATCH (a:Kind1|Kind2 {prop: 'val'})-[r:Rel1|Rel2*1..3]->(b)<-[:Rel]-(c)
[, MATCH (...)]?
[OPTIONAL MATCH (...)]?
WHERE  a.name = 'X'
   AND b.kind IN ['Function','Method']
   AND r.confidence > 0.8
   AND NOT a.filePath CONTAINS 'test'
   AND b.name =~ '.*Handler$'
   OR  c.name STARTS WITH 'Foo'
[WITH a, COUNT(r) AS hits WHERE hits > 2]?
RETURN DISTINCT a.name, b.kind, COUNT(*) AS n, r.reason
[ORDER BY n DESC, a.name]?
[SKIP 0]? [LIMIT 100]?
[UNION [ALL]? <second-query>]?
```

- Pattern: multi-node chain、label alternation `:A|B`、變長邊 `*min..max`、雙向 `<-` / `->` / `-`
- Inline node props `{name: 'X'}` 等價於 WHERE clause
- WHERE operators: `= <> < <= > >= AND OR NOT IN STARTS WITH ENDS WITH CONTAINS =~`
- Properties:
  - node: `.name`、`.kind`、`.filePath`、`.uid`、`.content`（lazy 讀檔）
  - edge: `.confidence`、`.reason`、`.rel_type`
- 字面值: string `'...'` `"..."`、整數、浮點、布林、`null`、list `[...]`
- 聚合: `COUNT(*)`、`COUNT(DISTINCT x)`、`SUM`、`MIN`、`MAX`、`AVG`、`COLLECT`
- `WITH` 簡化版: 變數投影 + 聚合 + alias，不支援 sub-pipeline 套疊
- `UNION` / `UNION ALL`、`DISTINCT`、`ORDER BY` / `SKIP` / `LIMIT`、`OPTIONAL MATCH`

### 明確排除

- `CREATE / DELETE / SET / REMOVE / MERGE`（graph 是 read-only mmap）
- `CALL` procedures、`FOREACH`
- `$param` 參數（直接內嵌字面值）
- `EXISTS { ... }` subquery、`CASE WHEN`
- Path 變數 `p = (a)-[*]->(b)`（用 multi-hop pattern 本身代替）

## 架構

### 模組結構

```
crates/cgn-core/src/cypher/
├── mod.rs        — 對外 API: parse(&str) -> Result<Query, CypherError>
│                              execute(&Query, &Graph, &repo_root) -> Result<QueryResult, CypherError>
├── lexer.rs      — Token stream（keyword case-insensitive、ident、string、number、symbol）
├── ast.rs        — Query / MatchClause / Pattern / NodePat / RelPat / Expr / Projection
├── parser.rs     — recursive descent，每個非終結符一個 fn，error 帶 byte offset
├── planner.rs    — AST → ExecPlan（每個 MATCH 一條 join chain）
├── executor.rs   — 跑 ExecPlan 對 ArchivedZeroCopyGraph，產 QueryResult
└── value.rs      — Value enum
```

`crates/cgn-cli/src/commands/cypher.rs` 退化成 ~50 LoC，只負責 CLI args、call `core::cypher::execute`、序列化為 JSON/TOON。

### AST 草圖

```rust
pub struct Query {
    pub matches: Vec<MatchClause>,
    pub where_: Option<Expr>,
    pub with: Option<WithClause>,
    pub return_: ReturnClause,
    pub order_by: Vec<OrderItem>,
    pub skip: Option<u64>,
    pub limit: Option<u64>,
    pub union: Option<Box<Query>>,
    pub union_all: bool,
}

pub struct MatchClause { pub optional: bool, pub patterns: Vec<Pattern> }
pub struct Pattern    { pub nodes: Vec<NodePat>, pub rels: Vec<RelPat> }  // nodes.len() == rels.len() + 1
pub struct NodePat    { pub var: Option<String>, pub kinds: Vec<NodeKind>, pub props: Vec<(String, Literal)> }
pub struct RelPat     { pub var: Option<String>, pub types: Vec<RelType>, pub range: Option<(u32, u32)>, pub dir: Direction }

pub enum Direction { Out, In, Both }

pub enum Expr {
    BinOp(Op, Box<Expr>, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
    Prop(String, String),     // (var, prop)
    Lit(Literal),
    In(Box<Expr>, Vec<Literal>),
    Regex(Box<Expr>, String),
    StartsWith(Box<Expr>, String),
    EndsWith(Box<Expr>, String),
    Contains(Box<Expr>, String),
    FunCall(String, Vec<Expr>),  // COUNT / SUM / ...
}

pub struct ReturnClause {
    pub distinct: bool,
    pub items: Vec<ReturnItem>,
}
pub struct ReturnItem {
    pub expr: ReturnExpr,        // Var(name) | Prop(var, prop) | FunCall | Star
    pub alias: Option<String>,
}
```

### Value 模型

```rust
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    NodeRef { idx: u32 },        // 序列化時展開 name/kind/filePath
    EdgeRef { src: u32, tgt: u32, rel_type: RelType, confidence: f32, reason: String },
}
```

### 執行模型

1. **Pattern matching**：每個 `MatchClause` 用 nested loop join：
   - 第一個 `NodePat` → 掃 `graph.nodes`，套 kind/inline-props filter
   - 每個 `RelPat` → 由當前 binding 走 `graph.out_offsets` / `in_offsets`，套 rel_type filter
   - 變數長度 `*min..max` → BFS（沿用 `graph_query::callees_of` / `callers_of`，新增 `bfs_with_dir` 支援 Both）
   - 結果是 `Vec<HashMap<&str, BindingValue>>`，其中 `BindingValue = Node(u32) | Edge(u32)`
2. **WHERE**：每個 binding eval `Expr`；list IN / regex / 比較
3. **OPTIONAL MATCH**：left-join；右側無 binding 時，左側保留、右側填 `Null`
4. **WITH**：當分隔器；做投影 + 聚合 + alias + 二次 WHERE
5. **RETURN**：產 columns + rows，`RETURN a` 自動展平為 `a.name`、`a.kind`、`a.filePath`
6. **DISTINCT**：對 rows 做 `HashSet`（用 Value 的 Eq+Hash impl）去重
7. **ORDER BY / SKIP / LIMIT**：`sort_by` + `iter().skip().take()`
8. **UNION (ALL)**：執行右邊 query，concat rows；無 ALL 則合併後去重

### `.content` 處理

保留現行 `ContentCache` + `slice_by_span`，搬到 `executor.rs`，作為 projection-time 解析；executor 收 `repo_root: PathBuf` 參數注入。

## 輸出格式

`QueryResult { columns: Vec<String>, rows: Vec<Vec<Value>> }`，CLI 端依 `--format` 序列化：

### JSON（預設，`--format json`）

```json
{
  "columns": ["a.name", "c.name", "c.filePath", "edge.confidence"],
  "rows": [
    ["caller", "callee", "src/edges.ts", 1.0],
    ["foo",    "bar",    "src/x.ts",     0.8]
  ]
}
```

### TOON（`--format toon`，LLM 友善）

```
columns: a.name, c.name, c.filePath, edge.confidence
rows[2]:
  caller, callee, src/edges.ts, 1.0
  foo, bar, src/x.ts, 0.8
```

TOON 對 50+ row 的輸出比 JSON 省 30-50% token。

### NodeRef / EdgeRef 展開

- `RETURN a` → columns 自動展開為 `a.name, a.kind, a.filePath`
- `RETURN a.name` → 單欄
- `RETURN r` → 展開為 `r.rel_type, r.confidence, r.reason`

## 錯誤模型

```rust
pub enum CypherError {
    Lex     { offset: usize, msg: String },
    Parse   { offset: usize, expected: String, found: String },
    Semantic{ msg: String },     // 未綁定變數、未知 NodeKind/RelType、未知 property
    Exec    { msg: String },     // type mismatch in r.confidence > 'foo'
}
```

CLI 印錯誤時顯示原 query 並用 `^` 標位置：

```
MATCH (a:Function)-[r:CALLZ]->(b) RETURN a, b
                       ^^^^^
ParseError: unknown RelType 'CALLZ' (expected one of: Defines, Imports, Calls, Extends, ...)
```

## 測試策略

### 新增分層測試

| 測試檔 | 範圍 |
|---|---|
| `core/src/cypher/parser_tests.rs` | grammar table-driven，每條 rule 至少 1 個 pass + 1 個 fail |
| `core/src/cypher/executor_tests.rs` | fixture graph（手刻 5 nodes / 6 edges），覆蓋 single-hop / multi-hop / variable-length / OPTIONAL / WHERE 各 operator / UNION / DISTINCT / LIMIT / ORDER BY / COUNT / IN / regex |
| `cli/tests/cypher_content.rs` | **改寫**：保留 3 個 e2e，assertion 從 `{source, target, edge}` 改為 `{columns, rows}` |
| `cli/tests/context_cypher_edge_metadata.rs` | **改寫**：edge metadata 改從 column-based row 抽取 |
| `cli/tests/cypher_multi_hop.rs` | **新增**：3-node chain、4-node chain、bidirectional |
| `cli/tests/cypher_aggregation.rs` | **新增**：COUNT/SUM/MIN/MAX、DISTINCT、ORDER BY、LIMIT、UNION |
| `cli/tests/cypher_toon_format.rs` | **新增**：`--format toon` 輸出對拍 |
| `cli/tests/cypher_error_messages.rs` | **新增**：每種 CypherError variant 一個 case，assert stderr 帶 `^` |

### Regression

- 現行 `cli_surface.rs` 對 `cypher --help` 的 single-repo 提示維持
- 多 repo gate `repo_selector` 維持

## 後續工作（out of scope）

- MCP 端透過 `core::cypher::parse + execute` 直接重用——不重寫 parser
- Query plan 優化（目前每個 MATCH 純 nested loop join；之後可加 hash join、index hints）
- `--explain` flag 印 ExecPlan
- 結構化 PATH 變數
