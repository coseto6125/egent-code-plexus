//! Route extraction precision regression suite.
//!
//! Each test parses a small inline source snippet and asserts the exact
//! `(method, path)` set extracted. The suite has two roles:
//!
//! - **Positive fixtures** pin idiomatic framework usage so a future query
//!   tweak that breaks recall fails loudly.
//! - **NEGATIVE fixtures** pin the FP classes that motivated this work
//!   (`dict.get("key")` / `Map.get(...)` / `headers.get(...)` etc.). They
//!   must extract **zero** routes — any emission is a false positive.
//!
//! Design rationale: `docs/superpowers/specs/2026-05-17-route-precision-design.md`.

use graph_nexus_analyzer::javascript::parser::JavaScriptProvider;
use graph_nexus_analyzer::python::parser::PythonProvider;
use graph_nexus_analyzer::typescript::parser::TypeScriptProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawRoute;

// ─── helpers ─────────────────────────────────────────────────────────

fn py_routes(src: &str) -> Vec<RawRoute> {
    PythonProvider::new()
        .unwrap()
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap()
        .routes
}

fn js_routes(src: &str) -> Vec<RawRoute> {
    JavaScriptProvider::new()
        .unwrap()
        .parse_file("test.js".as_ref(), src.as_bytes())
        .unwrap()
        .routes
}

fn ts_routes(src: &str) -> Vec<RawRoute> {
    TypeScriptProvider::new()
        .unwrap()
        .parse_file("test.ts".as_ref(), src.as_bytes())
        .unwrap()
        .routes
}

/// Normalize `(method, path)` for set-based comparison. Strips matching
/// surrounding quotes that tree-sitter `(string)` captures sometimes
/// carry through verbatim (Python / TS), so the test pins the *semantic*
/// route and not the syntactic quoting style.
fn pairs(routes: &[RawRoute]) -> Vec<(String, String)> {
    routes
        .iter()
        .map(|r| (r.method.to_uppercase(), strip_quotes(&r.path).to_string()))
        .collect()
}

fn strip_quotes(s: &str) -> &str {
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn assert_routes(actual: &[RawRoute], expected: &[(&str, &str)]) {
    let mut actual_pairs = pairs(actual);
    actual_pairs.sort();
    let mut expected_pairs: Vec<(String, String)> = expected
        .iter()
        .map(|(m, p)| (m.to_uppercase(), (*p).to_string()))
        .collect();
    expected_pairs.sort();
    assert_eq!(
        actual_pairs, expected_pairs,
        "route set mismatch\n  expected: {:?}\n  actual:   {:?}",
        expected_pairs, actual_pairs,
    );
}

fn assert_no_routes(actual: &[RawRoute], context: &str) {
    let count = actual.len();
    assert!(
        actual.is_empty(),
        "FP regression: expected 0 routes in {}, got {}: {:?}",
        context,
        count,
        pairs(actual),
    );
}

// ─── Python — FastAPI (literal `app`) ────────────────────────────────

#[test]
fn python_fastapi_app_extracts_idiomatic_routes() {
    let src = r#"
from fastapi import FastAPI

app = FastAPI()

@app.get("/users")
def list_users():
    pass

@app.post("/users")
def create_user():
    pass

@app.get("/users/{user_id}")
def get_user(user_id: int):
    pass

@app.delete("/users/{user_id}")
def delete_user(user_id: int):
    pass

@app.patch("/users/{user_id}")
def patch_user(user_id: int):
    pass
"#;
    assert_routes(
        &py_routes(src),
        &[
            ("GET", "/users"),
            ("POST", "/users"),
            ("GET", "/users/{user_id}"),
            ("DELETE", "/users/{user_id}"),
            ("PATCH", "/users/{user_id}"),
        ],
    );
}

// ─── Python — FastAPI APIRouter (custom identifier — S7) ─────────────

#[test]
fn python_fastapi_router_extracts_via_constructor_tracking() {
    // `router` is not literally `app` — gitnexus's hardcoded-receiver
    // approach would miss this. The S7 framework-constructor tracking
    // recognizes `router` as legitimate because the RHS is `APIRouter()`.
    let src = r#"
from fastapi import APIRouter

router = APIRouter()

@router.get("/items")
def list_items():
    pass

@router.post("/items")
def create_item():
    pass

@router.delete("/items/{item_id}")
def delete_item(item_id: int):
    pass
"#;
    assert_routes(
        &py_routes(src),
        &[
            ("GET", "/items"),
            ("POST", "/items"),
            ("DELETE", "/items/{item_id}"),
        ],
    );
}

// ─── Python — Flask app ──────────────────────────────────────────────

#[test]
fn python_flask_app_extracts_route_decorators() {
    let src = r#"
from flask import Flask

app = Flask(__name__)

@app.route("/", methods=["GET"])
def home():
    pass

@app.route("/login", methods=["POST"])
def login():
    pass

@app.get("/health")
def health():
    pass
"#;
    // Flask's `@app.route` uses a methods=[...] kwarg which the current
    // route_detector does not parse — only `@app.get/.post/...` shortcut
    // form is reliably extracted. Pin the reliable subset; expand in a
    // follow-up that adds Flask route(methods=...) parsing.
    assert_routes(&py_routes(src), &[("GET", "/health")]);
}

// ─── Python — Flask Blueprint (custom identifier — S7) ───────────────

#[test]
fn python_flask_blueprint_extracts_via_constructor_tracking() {
    let src = r#"
from flask import Blueprint

bp = Blueprint("users", __name__)

@bp.get("/users")
def list_users():
    pass

@bp.post("/users")
def create_user():
    pass

@bp.delete("/users/<int:id>")
def delete_user(id):
    pass
"#;
    assert_routes(
        &py_routes(src),
        &[
            ("GET", "/users"),
            ("POST", "/users"),
            ("DELETE", "/users/<int:id>"),
        ],
    );
}

// ─── Python — dict.get NEGATIVE fixture ──────────────────────────────

#[test]
fn python_dict_get_emits_zero_routes() {
    // This is the FP class that motivated the work. The file does not
    // import any HTTP framework — the framework-presence gate (S2) must
    // suppress every emission regardless of how plausibly the calls
    // pattern-match the old generic query.
    let src = r#"
import json

def process(payload):
    first = payload.get("source", {})
    name = first.get("name")
    target = first.get("target", {})
    tree = target.get("tree")
    # Headers-shaped lookups are equally FP-prone.
    auth = request_headers.get("Authorization")
    trace = request_headers.get("x-trace-id")
    return {"class_name": name, "tree": tree}
"#;
    assert_no_routes(&py_routes(src), "dict.get / headers.get pattern");
}

// ─── Python — call patterns without framework import NEGATIVE ────────

#[test]
fn python_app_get_without_framework_import_emits_zero() {
    // A user happens to name a variable `app` and call `.get(...)` — but
    // no fastapi/flask/django import is present. Must not emit any Route.
    let src = r#"
class FakeApp:
    def get(self, key, default=None): return default

app = FakeApp()
value = app.get("/users")
other = app.get("/items", None)
"#;
    assert_no_routes(&py_routes(src), "app-shaped name without framework import");
}

// ─── JavaScript — Express app ────────────────────────────────────────

#[test]
fn js_express_app_extracts_routes() {
    let src = r#"
const express = require('express');
const app = express();

app.get('/users', listUsers);
app.post('/users', createUser);
app.get('/users/:id', getUser);
app.delete('/users/:id', deleteUser);
app.patch('/users/:id', patchUser);
"#;
    assert_routes(
        &js_routes(src),
        &[
            ("GET", "/users"),
            ("POST", "/users"),
            ("GET", "/users/:id"),
            ("DELETE", "/users/:id"),
            ("PATCH", "/users/:id"),
        ],
    );
}

// ─── JavaScript — Express Router (custom identifier — S7) ────────────

#[test]
fn js_express_router_extracts_via_constructor_tracking() {
    let src = r#"
const express = require('express');
const router = express.Router();

router.get('/items', listItems);
router.post('/items', createItem);
router.delete('/items/:id', deleteItem);
"#;
    assert_routes(
        &js_routes(src),
        &[
            ("GET", "/items"),
            ("POST", "/items"),
            ("DELETE", "/items/:id"),
        ],
    );
}

// ─── JavaScript — Map/headers NEGATIVE fixture ───────────────────────

#[test]
fn js_map_and_headers_get_emit_zero_routes() {
    let src = r#"
const cache = new Map();
const headers = new Headers();

function handle(req) {
    const cached = cache.get('user-42');
    const auth = headers.get('Authorization');
    const trace = headers.get('x-trace-id');
    const sessionId = req.cookies.get('sid');
    return { cached, auth, trace, sessionId };
}
"#;
    assert_no_routes(&js_routes(src), "Map/Headers/cookies .get pattern");
}

// ─── TypeScript — object.get without framework import NEGATIVE ───────

#[test]
fn ts_no_framework_import_emits_zero_routes() {
    let src = r#"
interface KV { get(key: string): string | undefined }

class Store implements KV {
    private m = new Map<string, string>();
    get(key: string): string | undefined { return this.m.get(key); }
    set(key: string, val: string): void { this.m.set(key, val); }
}

const s = new Store();
const a = s.get('source');
const b = s.get('class_name');
const c = s.get('method_name');
"#;
    assert_no_routes(
        &ts_routes(src),
        "Store.get / Map.get without framework import",
    );
}
