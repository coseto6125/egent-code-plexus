//! Flask Blueprint shorthand route decorators — `@bp.get("/path")` /
//! `@bp.post(...)` / `@bp.put(...)` etc. exposed on a Blueprint instance.
//!
//! These are real route registrations but the method names (`get`/`post`)
//! collide with `dict.get(key)` semantics, so we previously gated route
//! emission on either:
//!   1. `has_any_http_framework` (file has `from flask import ...` etc.), or
//!   2. method ∈ {route, add_route, add_url_rule, add_api_route}
//!
//! Files that import a Blueprint transitively (`from . import bp`) without
//! `from flask import` directly — common in test apps and apps split into
//! sub-modules — failed both gates and silently dropped all `@bp.get` /
//! `@bp.post` decorator routes.
//!
//! The fix recognises that a `@expr(...)` decorator never wraps `dict.get`,
//! `requests.get`, or any other non-route HTTP-verb call; if the call sits
//! inside a `(decorator ...)` parent in the AST, it's safe to emit as a
//! route even without explicit framework imports.
//!
//! Regression for the parity gap surfaced after the dump-pagination fix
//! (Round 76 follow-up).

use cgn_analyzer::python::parser::PythonProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PythonProvider::new().expect("PythonProvider init");
    p.parse_file(Path::new("test_app.py"), src.as_bytes())
        .expect("parse_file")
}

fn routes(g: &LocalGraph) -> Vec<(&str, &str)> {
    g.routes.iter()
        .map(|r| (r.method.as_str(), r.path.as_str()))
        .collect()
}

#[test]
fn blueprint_get_decorator_emits_route_without_flask_import() {
    // Repro of `tests/test_apps/blueprintapp/apps/admin/__init__.py` pattern:
    // a sub-module imports the blueprint via `from . import bp` (transitive)
    // and uses `@bp.get(...)` — no `from flask import` in this file.
    let src = r#"
from . import bp

@bp.get("/index2")
def index2():
    return "ok"
"#;
    let g = parse(src);
    let rs = routes(&g);
    assert!(rs.iter().any(|(m, p)| *m == "get" && *p == "/index2"),
        "expected GET /index2 in {rs:?}");
}

#[test]
fn blueprint_post_decorator_emits_route_without_flask_import() {
    let src = r#"
from . import bp

@bp.post("/block")
def block():
    return "ok"
"#;
    let g = parse(src);
    let rs = routes(&g);
    assert!(rs.iter().any(|(m, p)| *m == "post" && *p == "/block"),
        "expected POST /block in {rs:?}");
}

#[test]
fn multiple_blueprint_decorators_each_emit() {
    // Real Flask 2.0+ Blueprint pattern: get/post/put/delete/patch shorthand.
    let src = r#"
from . import bp

@bp.get("/result/<id>")
def result(id):
    return id

@bp.post("/add")
def add():
    return "ok"

@bp.put("/<int:id>/update")
def update(id):
    return "ok"

@bp.delete("/<int:id>/delete")
def delete(id):
    return "ok"
"#;
    let g = parse(src);
    let rs = routes(&g);
    for (method, path) in [
        ("get", "/result/<id>"),
        ("post", "/add"),
        ("put", "/<int:id>/update"),
        ("delete", "/<int:id>/delete"),
    ] {
        assert!(rs.iter().any(|(m, p)| *m == method && *p == path),
            "expected {method} {path} in {rs:?}");
    }
}

#[test]
fn dict_get_call_does_not_emit_route() {
    // Regression guard: `obj.get("key")` outside a decorator MUST NOT emit a
    // route. The decorator-context check is exactly what disambiguates this.
    let src = r#"
def f():
    cfg = {"name": "x"}
    val = cfg.get("/looks/like/path")
    return val
"#;
    let g = parse(src);
    let rs = routes(&g);
    assert!(rs.is_empty(), "dict.get must not emit route: {rs:?}");
}

#[test]
fn requests_get_call_does_not_emit_route() {
    // Regression guard: `requests.get(url)` is an HTTP client call, not a
    // route registration. Must not emit.
    let src = r#"
import requests

def fetch():
    response = requests.get("/api/v1/data")
    return response.json()
"#;
    let g = parse(src);
    let rs = routes(&g);
    assert!(rs.is_empty(), "requests.get must not emit route: {rs:?}");
}

#[test]
fn classic_flask_route_decorator_still_emits() {
    // Regression guard: the legacy `@app.route(...)` path must keep working.
    // It's already gated by REGISTRATION_METHOD_NAMES so this didn't depend
    // on the new decorator check, but the new condition is OR'd so should
    // never inhibit existing emission.
    let src = r#"
from flask import Flask
app = Flask(__name__)

@app.route("/")
def index():
    return "hi"
"#;
    let g = parse(src);
    let rs = routes(&g);
    assert!(rs.iter().any(|(_, p)| *p == "/"), "{rs:?}");
}

#[test]
fn fastapi_router_decorator_emits_route() {
    // FastAPI's APIRouter exposes the same shorthand (`@router.get`,
    // `@router.post`, ...) — same root cause, same fix.
    let src = r#"
from . import router

@router.get("/items/{id}")
async def read_item(id: int):
    return {"id": id}
"#;
    let g = parse(src);
    let rs = routes(&g);
    assert!(rs.iter().any(|(m, p)| *m == "get" && *p == "/items/{id}"),
        "expected FastAPI route in {rs:?}");
}
