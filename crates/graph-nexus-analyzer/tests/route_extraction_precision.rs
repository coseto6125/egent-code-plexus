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
use graph_nexus_analyzer::php::parser::PhpProvider;
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

/// Parse Python with a caller-supplied file path. Use when the test
/// depends on the file being classified as `FileCategory::Test` via
/// `is_test_path` (e.g. path contains `/tests/` or matches
/// `*_test.py` / `conftest.` / `test_*` conventions).
fn py_routes_at(path: &str, src: &str) -> Vec<RawRoute> {
    PythonProvider::new()
        .unwrap()
        .parse_file(path.as_ref(), src.as_bytes())
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

fn php_routes(src: &str) -> Vec<RawRoute> {
    PhpProvider::new()
        .unwrap()
        .parse_file("test.php".as_ref(), src.as_bytes())
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
    // `@app.route(path, methods=[METHOD])` is now parsed: the kwarg list
    // drives the emitted method per route, not a blanket GET default.
    // P1 review fix — previously this test pinned the broken behavior.
    assert_routes(
        &py_routes(src),
        &[("GET", "/"), ("POST", "/login"), ("GET", "/health")],
    );
}

// ─── Python — Flask methods=[GET, POST] emits both ───────────────────

#[test]
fn python_flask_route_with_multiple_methods_emits_one_per_method() {
    let src = r#"
from flask import Flask

app = Flask(__name__)

@app.route("/api/items", methods=["GET", "POST"])
def items():
    pass
"#;
    assert_routes(
        &py_routes(src),
        &[("GET", "/api/items"), ("POST", "/api/items")],
    );
}

// ─── Python — Flask @app.route methods= unparseable → skip ───────────

#[test]
fn python_flask_route_with_dynamic_methods_skips_emission() {
    // When `methods=` is present but isn't a literal list of strings
    // (e.g. a variable, a function call, a constant reference), the
    // parser can't determine the HTTP method statically. Skipping
    // emission is the correct behavior — fabricating GET would
    // mis-classify the endpoint.
    let src = r#"
from flask import Flask

app = Flask(__name__)

ALLOWED = ["POST", "PUT"]

@app.route("/api/dynamic", methods=ALLOWED)
def dynamic_handler():
    pass
"#;
    // The only emission should be from the @app.get/post/... shortcut
    // form below. The @app.route above is dropped.
    let routes = py_routes(src);
    assert!(
        routes.is_empty(),
        "expected no routes when methods= is non-literal, got: {:?}",
        pairs(&routes)
    );
}

// ─── Python — custom CustomRouter class without imports NEGATIVE ─────

#[test]
fn python_custom_router_class_without_imports_emits_zero() {
    // FP regression guard for the route-method gate relaxation. A
    // self-contained script that defines its OWN class with a `.route`
    // method must NOT pass the gate — `.route(string)` calls in such
    // files are user code, not framework registration.
    //
    // The defensive gate `!imports.is_empty()` short-circuits because
    // a real Flask Blueprint usage always imports the blueprint
    // identifier (e.g. `from app.api import bp`). A bare class
    // definition with zero imports is a sufficient negative signal.
    let src = r#"
class CustomRouter:
    def route(self, path, methods=None):
        return None

    def add_url_rule(self, path, endpoint, view_func):
        return None

r = CustomRouter()
r.route("/api/foo", methods=["POST"])
r.add_url_rule("/api/bar", "bar", lambda: None)
"#;
    assert_no_routes(
        &py_routes(src),
        "CustomRouter class with route/add_url_rule methods, no imports",
    );
}

// ─── PHP — chained call with router-DSL intermediates POSITIVE ───────

#[test]
fn php_laravel_chain_with_router_dsl_intermediates_extracts() {
    // Defensive: validate that the walker handles real Laravel router
    // DSL chains where intermediate methods are router-builder methods
    // (`middleware`, `name`, `prefix`, `where`, `domain`, `controller`)
    // rather than the verb-method directly off `Route::middleware`. The
    // chain depth doesn't matter — the walker recurses through every
    // `member_call_expression` until it reaches the scoped_call root.
    let src = r#"<?php
        use Illuminate\Support\Facades\Route;

        Route::middleware(['auth'])->name('admin.')->get('/admin/users', [AdminController::class, 'index']);
        Route::prefix('api')->middleware(['auth:sanctum'])->name('api.')->post('/items', [ItemController::class, 'store']);
        Route::domain('admin.example.com')->controller(AdminController::class)->get('/dashboard', 'show');
    "#;
    assert_routes(
        &php_routes(src),
        &[
            ("GET", "/admin/users"),
            ("POST", "/items"),
            ("GET", "/dashboard"),
        ],
    );
}

// ─── Python — `.test_client.X(...)` chained calls NEGATIVE (parser filter) ──

#[test]
fn python_app_test_client_chained_calls_emit_zero() {
    // `app.test_client.get(...)` / `self.app.test_client.post(...)` are
    // test-client REQUESTS, not route DEFINITIONS. The parser-side filter
    // drops them by detecting `.test_client.` in the chained function name
    // (impossible to distinguish at tree-sitter call-shape level — they
    // look identical to `app.get(...)`).
    //
    // Empirical impact: this removes ~88% of the FPs that surfaced when
    // running gnx with `--include-tests` on sanic-org/sanic. A user who
    // names a production attribute `test_client` would be false-negative,
    // but `test_client` is overwhelmingly a testing convention.
    let src = r#"
from sanic import Sanic

app = Sanic("api")

# Real test-client REQUESTS, not route definitions. Must NOT emit.
_, response = app.test_client.get("/api/foo")
_, response = app.test_client.post("/api/items", data={"name": "x"})
_, response = app.test_client.delete("/api/items/42")
_, response = await app.asgi_client.get("/api/asgi")
_, response = app.sync_client.put("/api/sync")
"#;
    assert_no_routes(
        &py_routes(src),
        ".test_client / .asgi_client / .sync_client chained-call test-client requests",
    );
}

// ─── Python — pytest-fixture direct receiver in test file NEGATIVE ────

#[test]
fn python_test_file_direct_client_receiver_emits_zero() {
    // Real-world Sanic pattern (tests/worker/test_inspector.py): pytest
    // fixture `http_client` injected as function param, then called as
    // `http_client.post('/reload')`. Indistinguishable from a real
    // `app.post('/reload')` by call shape, but the FILE PATH gives the
    // signal — `tests/worker/test_inspector.py` is classified as
    // `FileCategory::Test`. Inside test files, a bare-identifier
    // receiver matching one of the conventional test-client variable
    // names (`http_client`, `client`, `api_client`, ...) gets dropped.
    //
    // Production files are unaffected — there `client = APIRouter()`
    // is legitimate and keeps emitting.
    let src = r#"
import pytest
from sanic import Sanic

@pytest.fixture
def http_client(inspector):
    return inspector.client

def test_run_inspector_reload(http_client):
    _, response = http_client.post("/reload")

def test_run_inspector_scale(http_client):
    _, response = http_client.post("/scale", json={"replicas": 4})

def test_using_generic_client(client):
    _, response = client.post("/api/foo")
"#;
    // Must NOT emit when path classifies as test.
    assert_no_routes(
        &py_routes_at("tests/worker/test_inspector.py", src),
        "pytest-fixture direct client receiver in test file",
    );
}

#[test]
fn python_production_file_direct_client_receiver_still_extracts() {
    // Symmetric guard: in a PRODUCTION file path (not classified as
    // test), `client` may legitimately be an `APIRouter` / `Blueprint`
    // / similar — keep emitting. The test-file gate prevents the
    // FN from spreading to production.
    //
    // Use Flask Blueprint `client.route(...)` form (the `route` method
    // bypasses the framework-presence requirement because it's in the
    // route-registration method allowlist; the file still has imports
    // so the `!imports.is_empty()` guard passes).
    let src = r#"
from flask import Blueprint

client = Blueprint("api", __name__)

@client.route("/api/foo", methods=["GET"])
def handle_foo():
    pass
"#;
    let routes = py_routes_at("src/app/api.py", src);
    let pairs_set: std::collections::BTreeSet<_> = pairs(&routes).into_iter().collect();
    assert!(
        pairs_set.contains(&("GET".to_string(), "/api/foo".to_string())),
        "production file must still emit routes on `client` receiver: {:?}",
        pairs_set
    );
}

// ─── Python — direct `client.X(...)` is indistinguishable, documented ──

#[test]
fn python_direct_client_get_emits_route_documented_limitation() {
    // The `.test_client.` chained-access filter does NOT catch direct
    // single-level `client.get('/x')` calls (where `client` is e.g.
    // `SanicTestClient(app)`). Tree-sitter sees the same shape as
    // `app.get('/x')` and the `client` variable's type isn't tracked.
    //
    // This is a documented limitation. The `--include-tests` flag and
    // `FileCategory::Test` classification at index/query time are the
    // mitigation — `client = SanicTestClient(...)` patterns live in
    // test files which get filtered by default in `gnx routes`.
    let src = r#"
from sanic_testing.testing import SanicTestClient
from sanic import Sanic

app = Sanic("api")
client = SanicTestClient(app)

# Indistinguishable at parser level from a real `app.get('/x')` route.
client.get("/api/foo")
"#;
    let routes = py_routes(src);
    let pairs_set: std::collections::BTreeSet<_> = pairs(&routes).into_iter().collect();
    assert!(
        pairs_set.contains(&("GET".to_string(), "/api/foo".to_string())),
        "expected parser to emit direct client.get (mitigated at query time): {:?}",
        pairs_set
    );
}

// ─── Python — Sanic raw-string + bare-path + frozenset methods ───────

#[test]
fn python_sanic_bare_path_and_raw_string_and_frozenset_extracts() {
    // Real-world Sanic patterns previously missed (recovered by the
    // PR-final follow-ups, validated against sanic-org/sanic):
    //
    // 1. `@app.route("path/<x>/y")` — bare path (no leading slash).
    //    Sanic accepts both forms; `clean_route_path_lax` normalizes.
    // 2. `@app.route(r"/path/<x:regex>")` — raw-string prefix `r"..."` so
    //    backslashes in the type spec don't escape. `strip_string_quotes`
    //    now skips Python string prefixes (r/b/f/u).
    // 3. `methods=frozenset({"PUT","POST"})` — frozenset wrapper around a
    //    set literal. `extract_methods_kwarg` unwraps frozenset/set/tuple
    //    /list calls and walks the inner collection for string literals.
    let src = r#"
from sanic import Sanic

app = Sanic("api")

@app.route("path/<possibly_neg:int>/another-word")
async def bare_path_handler(request, possibly_neg):
    pass

@app.route(r"/path/to/<ext:file\.(txt)>")
async def regex_path_handler(request, ext):
    pass

@app.route("/request_path", methods=frozenset({"PUT", "POST"}), version=2)
async def frozenset_methods_handler(request):
    pass
"#;
    assert_routes(
        &py_routes(src),
        &[
            ("GET", "/path/<possibly_neg:int>/another-word"),
            ("GET", "/path/to/<ext:file\\.(txt)>"),
            ("PUT", "/request_path"),
            ("POST", "/request_path"),
        ],
    );
}

// ─── Python — Flask transitive Blueprint import (recall fix) ─────────

#[test]
fn python_flask_blueprint_transitive_import_extracts() {
    // Real-world Flask pattern that gnx previously missed (recall gap
    // found via gitnexus cross-validation against miguelgrinberg/microblog
    // `app/api/tokens.py`): a sub-module file does `from app.api import
    // bp` (no direct `from flask import ...`) and decorates handlers
    // with `@bp.route('/x', methods=[...])`. The framework-import gate
    // can't see through the transitive chain, so the relaxation here
    // allows route-registration method names (`route`, `add_route`,
    // `add_url_rule`, `add_api_route`) to bypass the gate. These method
    // names are sufficiently framework-specific that the bypass is safer
    // than the recall loss.
    let src = r#"
from app.api import bp

@bp.route("/tokens", methods=["POST"])
def get_token():
    return {"token": "..."}

@bp.route("/tokens", methods=["DELETE"])
def revoke_token():
    return "", 204
"#;
    assert_routes(
        &py_routes(src),
        &[("POST", "/tokens"), ("DELETE", "/tokens")],
    );
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

// ─── Python — Sanic (user-requested framework) ───────────────────────

#[test]
fn python_sanic_app_extracts_routes() {
    let src = r#"
from sanic import Sanic

app = Sanic("api")

@app.get("/items")
async def list_items(request):
    pass

@app.post("/items")
async def create_item(request):
    pass

@app.delete("/items/<item_id>")
async def delete_item(request, item_id):
    pass

@app.route("/health")
async def health(request):
    pass
"#;
    // `@app.route("/health")` (no methods kwarg) translates to GET; same
    // path-translation rule as Flask. `<item_id>` is Sanic's path-param
    // syntax — accepted by the path-shape filter because the literal
    // starts with `/`.
    assert_routes(
        &py_routes(src),
        &[
            ("GET", "/items"),
            ("POST", "/items"),
            ("DELETE", "/items/<item_id>"),
            ("GET", "/health"),
        ],
    );
}

// ─── Python — `add_route` family (Sanic / Flask / FastAPI imperative form) ──

#[test]
fn python_sanic_add_route_extracts_route() {
    // `app.add_route(handler, '/path')` — Sanic-style imperative route
    // registration. The handler is the 1st arg, path is the 2nd; the
    // generic query captures the first string in argument_list which is
    // the path either way.
    let src = r#"
from sanic import Sanic

app = Sanic("api")

async def list_items(request):
    pass

app.add_route(list_items, "/items")
"#;
    assert_routes(&py_routes(src), &[("GET", "/items")]);
}

#[test]
fn python_flask_add_url_rule_extracts_route() {
    // `app.add_url_rule('/path', endpoint, view_func)` — Flask-style.
    // Defaults to GET; methods=[...] kwarg parsing is a follow-up.
    let src = r#"
from flask import Flask

app = Flask(__name__)

def home():
    pass

app.add_url_rule("/home", "home", home)
app.add_url_rule("/about", "about", about_view)
"#;
    assert_routes(&py_routes(src), &[("GET", "/home"), ("GET", "/about")]);
}

#[test]
fn python_fastapi_add_api_route_extracts_route() {
    // `app.add_api_route('/path', endpoint)` — FastAPI imperative form.
    let src = r#"
from fastapi import FastAPI

app = FastAPI()

async def get_items():
    pass

app.add_api_route("/items", get_items)
app.add_api_route("/items/{item_id}", get_item)
"#;
    assert_routes(
        &py_routes(src),
        &[("GET", "/items"), ("GET", "/items/{item_id}")],
    );
}

// ─── Python — Sanic Blueprint (custom identifier) ────────────────────

#[test]
fn python_sanic_blueprint_extracts_via_framework_gate() {
    let src = r#"
from sanic import Blueprint

users_bp = Blueprint("users", url_prefix="/users")

@users_bp.get("/")
async def list_users(request):
    pass

@users_bp.post("/")
async def create_user(request):
    pass
"#;
    assert_routes(&py_routes(src), &[("GET", "/"), ("POST", "/")]);
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

// ─── PHP — Laravel routes (bare + slash-prefixed paths) ──────────────

#[test]
fn php_laravel_routes_extract_bare_and_slash_paths() {
    // Laravel routes accept paths with OR without a leading slash —
    // `Route::get('register', ...)` and `Route::get('/users', ...)` are
    // both valid. The PHP parser uses a `route.scope` allowlist
    // (Route/Router/Slim/App/Symfony/Lumen) and a framework import gate
    // (Illuminate/Laravel/Slim/Symfony/etc.) instead of a path-shape
    // filter. Bare paths are normalized to leading-slash form so the
    // builder's `looks_like_path` predicate (which requires `/`) doesn't
    // drop them downstream.
    let src = r#"<?php
        use Illuminate\Support\Facades\Route;

        Route::get('/users', [UserController::class, 'index']);
        Route::post('/users', [UserController::class, 'store']);
        Route::get('register', [RegisteredUserController::class, 'create']);
        Route::post('forgot-password', [PasswordResetLinkController::class, 'store']);
        Route::delete('users/{id}', [UserController::class, 'destroy']);
    "#;
    assert_routes(
        &php_routes(src),
        &[
            ("GET", "/users"),
            ("POST", "/users"),
            ("GET", "/register"),
            ("POST", "/forgot-password"),
            ("DELETE", "/users/{id}"),
        ],
    );
}

// ─── PHP — Laravel chained Route::middleware()->get(...) (recall fix) ─

#[test]
fn php_laravel_chained_middleware_then_verb_extracts() {
    // Real-world Laravel pattern previously missed: the route registration
    // is chained off a `Route::middleware(...)` call, expressed in tree-
    // sitter as `member_call_expression` whose object is a
    // `scoped_call_expression`. A dedicated query catches this shape and
    // the parser walks the object chain to verify the root scope is in
    // the router allowlist (so `$cache->get('key')` still doesn't FP).
    let src = r#"<?php
        use Illuminate\Support\Facades\Route;

        Route::middleware(['auth:sanctum'])->get('/user', function (Request $request) {
            return $request->user();
        });

        Route::middleware(['auth'])->prefix('admin')->post('/users', [AdminController::class, 'create']);
    "#;
    assert_routes(&php_routes(src), &[("GET", "/user"), ("POST", "/users")]);
}

// ─── PHP — chained .get() on non-router object NEGATIVE ──────────────

#[test]
fn php_chained_get_on_non_router_emits_zero() {
    // Defensive: the chained query is broad (`member_call_expression`),
    // so a `$cache->get('/key')` must NOT fire. The parser walks the
    // object chain inward; if the root isn't a scoped_call to a router
    // class, no emit.
    let src = r#"<?php
        use Illuminate\Support\Facades\Route;

        $cache = new Cache();
        $value = $cache->get('/some-key');
        $other = $cache->fluent()->get('/another-key');
    "#;
    assert_no_routes(
        &php_routes(src),
        "chained .get on $cache (no router scope root)",
    );
}

// ─── PHP — Laravel nested middleware group + bare paths (recall fix) ──

#[test]
fn php_laravel_nested_middleware_group_routes_extract() {
    // Real-world Laravel pattern that gnx previously missed (PR #50
    // review finding): routes declared inside `Route::middleware(...)
    // ->group(function () { ... })` with bare paths and chained
    // `->name(...)`. Matches the breeze stubs structure verbatim.
    let src = r#"<?php
        use Illuminate\Support\Facades\Route;

        Route::middleware('auth')->group(function () {
            Route::get('confirm-password', [ConfirmablePasswordController::class, 'show'])
                ->name('password.confirm');
            Route::post('confirm-password', [ConfirmablePasswordController::class, 'store']);
            Route::put('password', [PasswordController::class, 'update'])
                ->name('password.update');
            Route::get('reset-password/{token}', [NewPasswordController::class, 'create']);
            Route::get('verify-email', EmailVerificationPromptController::class);
        });
    "#;
    assert_routes(
        &php_routes(src),
        &[
            ("GET", "/confirm-password"),
            ("POST", "/confirm-password"),
            ("PUT", "/password"),
            ("GET", "/reset-password/{token}"),
            ("GET", "/verify-email"),
        ],
    );
}

// ─── PHP — Route::get without Illuminate import NEGATIVE (P1 review) ─

#[test]
fn php_route_get_without_illuminate_import_emits_zero() {
    // Reviewer-supplied regression case. A user-defined `class Route` or
    // any non-Laravel `Route::method(...)` pattern matched the scope-name
    // allowlist alone, producing fake routes. The framework-presence
    // gate (must import from Illuminate / Slim / Symfony / etc.) blocks
    // these.
    let src = r#"<?php
        // No `use Illuminate\...;` — bare `Route::` is a user class call.
        class Route {
            public static function get($key, $default = null) { return $default; }
        }
        $value = Route::get('cache-key');
    "#;
    assert_no_routes(&php_routes(src), "Route::get without Illuminate import");
}

// ─── PHP — facade .get() NEGATIVE ────────────────────────────────────

#[test]
fn php_facade_get_emits_zero_routes() {
    // `Cache::get / Config::get / Auth::get` are common Laravel facade
    // calls that match the same `scoped_call_expression` shape as
    // `Route::get`. The router-class allowlist must filter them out.
    let src = r#"<?php
        $value = Cache::get('user-cache-key');
        $cfg = Config::get('app.name');
        $user = Auth::get('current');
        $log = Log::get('channel');
        $store = Storage::get('file.txt');
    "#;
    assert_no_routes(
        &php_routes(src),
        "Cache/Config/Auth/Log/Storage::get facades without router scope",
    );
}
