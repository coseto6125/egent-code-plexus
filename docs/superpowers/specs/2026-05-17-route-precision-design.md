# Route Extraction Precision — Design Spec

**Status**: approved 2026-05-17
**Branch**: `feat/route-precision`
**Predecessor**: PR #49 (concurrency audit) — pinned parallel-emit invariants
**Sub-project**: 1 of 7 — NodeKind / RelType parity vs gitnexus

## Problem

`cgn admin index --repo .` on this repo produces **49 Route nodes, 42 of which are false positives (86% FP rate)**. The FPs all come from one shape: `dict.get("key")` / `Map.get("x")` / `headers.get("y")` style calls being matched by an under-constrained tree-sitter query.

Root cause is structural — five per-language `queries.scm` files (python / typescript / javascript / ruby / php) each contain a generic route capture with **no constraint on the call receiver**:

```scheme
(call
  function: (attribute attribute: (identifier) @route.method
    (#match? @route.method "^(get|post|put|delete|patch|...)$"))
  arguments: (argument_list (string) @route.path))
```

A second-tier bug in `route_detector.rs:13-20` (`looks_like_path` accepts almost anything) and `route_detector.rs:57` (`method.to_lowercase().contains(m)` substring match) compound the noise but are subordinate to the query problem.

Comparison to gitnexus is **not** a strict over/under judgement — gitnexus's framework-gated extractors miss real routes when the user names their FastAPI app something other than literally `app`. Our goal is **maximum precision without sacrificing recall on idiomatic framework usage**, not "match gitnexus's number".

## Goals

- **Precision target**: ≥95% on framework-idiomatic code (committed fixtures), ≥98% on the cgn self-corpus.
- **Recall preservation**: idiomatic FastAPI / Flask / Django / Express / NestJS / Laravel patterns must still extract.
- **No hardcoded receiver allowlist**. Receiver legitimacy is established structurally — via framework-constructor tracking — not by enumerating identifier names.
- **User-tunable precision**: `--route-confidence high|certain|all` CLI flag with `high` as default.

## Non-goals

- Not chasing parity with gitnexus's count. Real route count is somewhere between 291 (gitnexus) and 4474 (current cgn) — neither is ground truth.
- Not adding new framework support in this PR beyond what existing parsers already attempt (Express, FastAPI, Flask, Django, NestJS basics, Laravel). New frameworks land in follow-up PRs.
- Not removing the `RawRoute` plumbing — only tightening the gates.

## What this PR ships (vs. what the original design proposed)

Implementation iteration revealed that the FP class can be eliminated with a much smaller change than the full multi-signal design first sketched below. Self-corpus measurement: 49 routes → 7 routes, 86% → 0% FP rate. All 10 precision-suite tests pass. The shipped surface is:

1. **Path-shape filter (`route_detector::clean_route_path`)** applied at parser-emit time. Only literals starting with `/` survive. This kills the dominant FP class (`Map.get("k")` / `headers.get("x")` / `dict.get("key")`) universally because none of those keys start with a slash.
2. **Python-only framework-presence gate**. The Python parser additionally requires the file to import one of `{fastapi, flask, django, starlette, aiohttp, tornado, sanic, bottle, falcon, pyramid, quart, litestar}` before emitting any generic route. This handles the edge case where a slash-prefixed string is passed to a non-HTTP method (e.g. `FakeApp.get("/users")` in code without any web-framework import).
3. **JS/TS skip the framework gate intentionally** because their parsers don't capture CommonJS `require('express')` as an import — gating would regress Node.js codebases. Path-shape filter alone reaches 0% FP on the JS/TS fixtures, and `Map.get("/literal-slash-key")` style residual-FP cases are mathematically possible but practically vanishing.

Deferred to follow-ups (kept here for traceability):

- **Removing the generic route block from 5 `queries.scm` files** — the path-shape filter renders the generic query effectively inert for non-routes, so wholesale removal is no longer load-bearing. Revisit if a measurable FP class survives.
- **Confidence stratification + `--route-confidence` CLI flag** — would let the user dial precision/recall. PR 1 ships with a single implicit "default" mode that hits 100% precision on self-corpus; the flag becomes useful when we have a real recall-leaning use case driving it.
- **Framework-constructor tracking (S7 in the original design)** — turns out unnecessary in PR 1 because existing framework-presence gates (`has_fastapi` / `has_express` / `has_flask` via import inspection) already authorise the `@router.get` / `@bp.route` patterns implicitly.
- **CommonJS `require()` recognition in the JS parser** — would tighten JS/TS to match Python's defense-in-depth. Independent change.

The rest of this document describes the original (now superseded) full design and is kept for future reference / extension.

## Approach: stratified-confidence + multi-signal voting

### Signal inventory

A Route emission requires multiple converging signals. No single signal is sufficient.

| ID | Signal | Captured how |
|----|--------|--------------|
| S1 | Path starts with `/` | `looks_like_path` tightened to `s.starts_with('/')` |
| S2 | File imports a known HTTP framework | Already tracked per-language (`has_fastapi` / `has_django` / `has_flask` / etc.); extend to `has_any_http_framework` |
| S3 | Call/decorator uses HTTP verb attribute | Existing `@route.method` capture |
| S4 | First arg is string literal | Existing `(string) @route.path` capture |
| S5 | Decorator immediately wraps a function definition | Existing FastAPI-style query already enforces this |
| S6 | Call form has callable arg (handler) | New: per-language query addition |
| S7 | Receiver is bound to a framework constructor (`x = APIRouter()`, `bp = Blueprint(...)`) **in the same file** | New: two-pass parser — pre-scan collects framework-constructor LHS identifiers, then main route capture cross-checks the receiver identifier against that set |

### Confidence tiers

```rust
pub enum RouteConfidence {
    Certain = 99,   // S1 + S2 + S5 + S7  (decorator + framework + receiver from ctor)
    High    = 90,   // S1 + S2 + S6        (call form + framework + handler arg)
    Low     = 70,   // S1 + S2             (path-shape + framework only)
}
```

`--route-confidence` CLI flag drops emissions below the threshold:
- `certain` → only Tier-Certain  (paranoid; expected ~7 routes on cgn)
- `high`    → Tier-Certain + High (default; expected ~10-15 on cgn)
- `all`     → all three           (recall-leaning; expected ~30-50 on cgn)

Files with **no HTTP framework import are never emitted Routes** under any threshold — S2 is mandatory. This single gate eliminates the `dict.get("key")` FP class universally.

### Why S7 is not a hardcoded allowlist

Hardcoded: `if receiver_name in ("app", "router", "bp"): allow`. Breaks the moment the user names their app `my_api`.

Structural (S7): in the same file, walk for assignments where RHS is a constructor call to a known **framework class** (`APIRouter`, `Blueprint`, `Flask`, `FastAPI`, `Starlette`, etc.) — and use **whatever LHS identifier the user chose** as the legitimate receiver. The framework-class list is a small (≤10 per language), stable, well-known set — not user-code-dependent.

## File-level changes

| File | Change |
|------|--------|
| `crates/cgn-analyzer/src/route_detector.rs` | `looks_like_path` → `s.starts_with('/')`; `detect_from_call` substring → exact verb match; add `confidence` field |
| `crates/cgn-core/src/analyzer/types.rs` | `RawRoute` gains `confidence: f32` (default 0.7) |
| `crates/cgn-analyzer/src/python/queries.scm` | Remove generic route block (lines 46-50) |
| `crates/cgn-analyzer/src/typescript/queries.scm` | Same |
| `crates/cgn-analyzer/src/javascript/queries.scm` | Same — keep the existing Express-specific framework query |
| `crates/cgn-analyzer/src/ruby/queries.scm` | Same |
| `crates/cgn-analyzer/src/php/queries.scm` | Same |
| `crates/cgn-analyzer/src/python/parser.rs` | Pre-scan pass to collect framework-constructor LHS identifiers; gate route emission on S2 + S7 |
| `crates/cgn-analyzer/src/typescript/parser.rs` | Same shape |
| `crates/cgn-analyzer/src/javascript/parser.rs` | Same shape |
| `crates/cgn-analyzer/src/resolution/builder.rs` | Pipe `confidence` through to Route node creation; honour `--route-confidence` threshold |
| `crates/code-graph-nexus/src/commands/admin/index.rs` (or wherever the index CLI args live) | New `--route-confidence` flag |
| `crates/cgn-analyzer/tests/fixtures/routes/` | New directory with positive + negative fixture files + `manifest.json` |
| `crates/cgn-analyzer/tests/route_extraction_precision.rs` | New test — runs every fixture, asserts emitted route set matches manifest |

## Verification strategy

**All verification is internal to the cgn repo. No external scripts under `scripts/`. No CI dependency on external repos.**

### Layer 1 — committed fixture tests (CI-blocking)

Small synthetic source files representing real framework idioms (positive) and FP triggers (negative), with explicit expected route lists in a `manifest.json`. Test asserts exact match.

Required fixtures (minimum for PR 1):
- `python_fastapi_app.py` (FastAPI literal `app = FastAPI()` + `@app.get/post/...`)
- `python_fastapi_router.py` (`router = APIRouter()` + `@router.get/...`)
- `python_flask_app.py` (`app = Flask(__name__)` + `@app.route(...)`)
- `python_flask_blueprint.py` (`bp = Blueprint(...)` + `@bp.route(...)`)
- `python_django_urlpatterns.py` (existing pattern coverage)
- `python_dict_get_NEGATIVE.py` (`dict.get / Map.get` heavy — expects 0 routes)
- `js_express_app.js` (`app = express()` + `app.get/...`)
- `js_express_router.js` (`router = express.Router()` + `router.get/...`)
- `js_map_headers_NEGATIVE.js` (Map/headers/object.get heavy — expects 0 routes)
- `ts_nest_controller.ts` (`@Controller @Get`)
- `ts_no_framework_NEGATIVE.ts` (no HTTP framework import — expects 0 routes)

### Layer 2 — self-corpus regression check (PR body, not committed test)

Run `cgn admin index --repo .` on cgn itself before + after. Assert in PR body:
- Before: 49 routes, 42 FP (86%)
- After (default `high`): ≤ 15 routes, ≤ 2 FP (≤ 15%)
- After (`certain`): ≤ 10 routes, 0 FP

### Layer 3 — OpenAPI-grounded sanity on 1-2 real repos (PR body, not committed)

Local-only one-shot during PR work. The framework's own `openapi.json` is the ground truth — no human counting bias.

Procedure:
1. `git clone tiangolo/full-stack-fastapi-template /tmp/fastapi-sample`
2. Generate openapi from the sample: `python -c "from app.main import app; import json; print(json.dumps(app.openapi()))"` → `/tmp/fastapi-sample.openapi.json`
3. Extract ground-truth route set: `jq -r '.paths | to_entries[] | .key as $p | .value | keys[] | "\(. | ascii_upcase) \($p)"' /tmp/fastapi-sample.openapi.json | sort`
4. `cp -r /tmp/fastapi-sample /tmp/cgn-sample && cd /tmp/cgn-sample && cgn admin index --repo . && cgn cypher "MATCH (n) WHERE n.kind='Route' RETURN n.name" --format json | jq -r '.rows[][]' | sort > /tmp/cgn-routes.txt`
5. `diff /tmp/cgn-routes.txt /tmp/openapi-routes.txt` → count FP / FN
6. Repeat for an Express sample (manual count if no openapi.json available, or use swagger-jsdoc-generated spec)

PR body posts:
- Repo + commit pin
- Ground-truth route count from openapi.json
- Cgn-extracted route count (default `high` + `certain` thresholds)
- Precision / Recall / F1

**No script is committed.** The procedure above lives in this spec only — anyone replicating runs it manually.

### Layer 4 — gitnexus cross-validation: **infeasible, explicitly skipped**

Cannot run gitnexus inside cgn CI (different runtime, dep conflicts, nondeterministic indexing). Cross-validation is a future-PR exercise if/when an isolation harness exists. Stated here so we don't silently drop it.

## Trade-offs accepted

- **Hand-written custom DSL frameworks** (in-house routing libraries that use bespoke class names not in our framework list) will no longer be extracted. Mitigation: users can use `--route-confidence all` to fall back to looser matching, at the cost of FPs returning.
- **Maintenance cost**: framework constructor list (one short array per language) needs updates when new mainstream frameworks emerge. List size kept ≤10 per language to bound cost.
- **One-time effort**: 11 fixture files + 1 test harness file + per-language parser pre-scan additions.

## Out of scope (deferred to follow-up PRs)

- C# `[HttpGet]` / `[Route]` attribute extraction (PR 3)
- Rails routes.rb / Sinatra global DSL (PR 2)
- Go gin / echo / chi / gorilla-mux specific patterns (PR 2)
- Laravel / Slim / Symfony PHP specifics (PR 2)
- Property under-extract fix for 8 languages (separate sub-project, after this lands)
