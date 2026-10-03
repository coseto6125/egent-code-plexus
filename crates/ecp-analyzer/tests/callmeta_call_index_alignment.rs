//! A `RawCallMeta` names its call by `(caller_span, call_index)`, an index into
//! the caller's `RawNode.calls`. The indirect-dispatch detectors derive that
//! index by counting call nodes in their own walk, so it only lands on the
//! right Calls edge while both walks see the same calls in the same order.
//! Each case puts the flagged call between direct calls (and after a
//! construction, which `RawNode.calls` records but a `call_expression`
//! counter does not see) and checks the meta points at the flagged call.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{CallSite, LocalGraph};
use std::path::Path;

fn parse(provider: &dyn LanguageProvider, path: &str, src: &str) -> LocalGraph {
    provider
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file")
}

/// The callee name each meta of `caller` points at, in meta order.
fn flagged_callees(g: &LocalGraph, caller: &str) -> Vec<String> {
    g.call_metas
        .iter()
        .filter(|m| m.caller_name == caller)
        .map(|m| {
            let node = g
                .nodes
                .iter()
                .find(|n| n.span == m.caller_span)
                .expect("meta caller span names a node");
            match node.calls.get(m.call_index as usize) {
                Some(raw) => CallSite::parse(raw).name().to_string(),
                None => format!("<index {} out of {:?}>", m.call_index, node.calls),
            }
        })
        .collect()
}

fn assert_points_at(g: &LocalGraph, caller: &str, expected: &str) {
    let got = flagged_callees(g, caller);
    let calls: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name == caller)
        .map(|n| &n.calls)
        .collect();
    assert!(
        !got.is_empty() && got.iter().all(|name| name.ends_with(expected)),
        "{caller}: metas point at {got:?}, expected `{expected}`; calls = {calls:?}"
    );
}

#[test]
fn javascript_callback_meta_points_at_flagged_call() {
    let p = JavaScriptProvider::new().expect("provider");
    let g = parse(
        &p,
        "a.js",
        "function between(cb) { a(); cb(); b(); }\n\
         function afterNew(cb) { new Widget(); cb(); }\n\
         function nested(cb) { a(cb()); }\n",
    );
    assert_points_at(&g, "between", "cb");
    assert_points_at(&g, "afterNew", "cb");
    assert_points_at(&g, "nested", "cb");
}

#[test]
fn typescript_callback_meta_points_at_flagged_call() {
    let p = TypeScriptProvider::new().expect("provider");
    let g = parse(
        &p,
        "a.ts",
        "function between(cb: () => void) { a(); cb(); b(); }\n\
         function afterNew(cb: () => void) { new Widget(); cb(); }\n",
    );
    assert_points_at(&g, "between", "cb");
    assert_points_at(&g, "afterNew", "cb");
}

#[test]
fn python_callback_meta_points_at_flagged_call() {
    let p = PythonProvider::new().expect("provider");
    let g = parse(
        &p,
        "a.py",
        "class Widget:\n    pass\n\n\
         def between(cb):\n    a()\n    cb()\n    b()\n\n\
         def after_new(cb):\n    Widget()\n    cb()\n\n\
         def nested(cb):\n    a(cb())\n",
    );
    assert_points_at(&g, "between", "cb");
    assert_points_at(&g, "after_new", "cb");
    assert_points_at(&g, "nested", "cb");
}

#[test]
fn c_fn_pointer_meta_points_at_flagged_call() {
    let p = CProvider::new().expect("provider");
    let g = parse(
        &p,
        "a.c",
        "void between(void (*fp)(int)) { a(); fp(1); b(); }\n\
         void nested(void (*fp)(int)) { a(fp(1)); }\n",
    );
    assert_points_at(&g, "between", "fp");
    assert_points_at(&g, "nested", "fp");
}

#[test]
fn cpp_fn_pointer_meta_points_at_flagged_call() {
    let p = CppProvider::new().expect("provider");
    let g = parse(
        &p,
        "a.cpp",
        "struct Widget {};\n\
         void between(void (*fp)(int)) { a(); fp(1); b(); }\n\
         void after_new(void (*fp)(int)) { Widget* w = new Widget(); fp(1); }\n",
    );
    assert_points_at(&g, "between", "fp");
    assert_points_at(&g, "after_new", "fp");
}

#[test]
fn rust_dyn_meta_points_at_flagged_call() {
    let p = RustProvider::new().expect("provider");
    let g = parse(
        &p,
        "a.rs",
        "trait Handler { fn run(&self); }\n\
         fn between(h: &dyn Handler) { a(); h.run(); b(); }\n\
         fn nested(h: &dyn Handler) { a(h.run()); }\n",
    );
    assert_points_at(&g, "between", "run");
    assert_points_at(&g, "nested", "run");
}
