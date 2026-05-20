use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::CallMeta;
use std::path::Path;

fn parse_py(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = PythonProvider::new().expect("PythonProvider::new");
    provider
        .parse_file(Path::new("test.py"), src.as_bytes())
        .expect("parse_file")
}

// ── Python: callback parameter call ────────────────────────────────────────

#[test]
fn python_callback_param_call_marked_callback() {
    let src = r#"
def process(items, callback):
    for item in items:
        callback(item)
"#;
    let g = parse_py(src);
    // `callback(item)` — callback is a parameter → FLAG_CALLBACK.
    let meta = g.call_metas.iter().find(|m| m.caller_name == "process");
    assert!(
        meta.is_some(),
        "expected RawCallMeta for callback param call in `process`; call_metas: {:?}",
        g.call_metas
    );
    let meta = meta.unwrap();
    assert_eq!(
        meta.flags & CallMeta::FLAG_CALLBACK,
        CallMeta::FLAG_CALLBACK,
        "callback(item) must set FLAG_CALLBACK"
    );
    assert_eq!(
        meta.flags & CallMeta::FLAG_DIRECT,
        0,
        "callback param call must NOT be direct"
    );
}

// ── Python: getattr dispatch ────────────────────────────────────────────────

#[test]
fn python_getattr_call_marked_dynamic() {
    let src = r#"
def dynamic_call(obj, method_name, arg):
    fn = getattr(obj, method_name)
    fn(arg)
"#;
    let g = parse_py(src);
    // `getattr(obj, method_name)` is a call to `getattr` → FLAG_DYNAMIC_DISPATCH.
    let meta = g
        .call_metas
        .iter()
        .find(|m| m.caller_name == "dynamic_call");
    assert!(
        meta.is_some(),
        "expected RawCallMeta for getattr() call; call_metas: {:?}",
        g.call_metas
    );
    let meta = meta.unwrap();
    assert_eq!(
        meta.flags & CallMeta::FLAG_DYNAMIC_DISPATCH,
        CallMeta::FLAG_DYNAMIC_DISPATCH,
        "getattr() call must set FLAG_DYNAMIC_DISPATCH"
    );
}

// ── Python: direct function call — no CallMeta ─────────────────────────────

#[test]
fn python_direct_call_no_callmeta() {
    let src = r#"
def helper(x):
    return x + 1

def main():
    result = helper(42)
    return result
"#;
    let g = parse_py(src);
    // `helper(42)` — `helper` is not a parameter, so no RawCallMeta.
    let metas: Vec<_> = g
        .call_metas
        .iter()
        .filter(|m| m.caller_name == "main")
        .collect();
    assert!(
        metas.is_empty(),
        "direct call must not produce RawCallMeta; got: {:?}",
        metas
    );
}

// ── Python: functools.partial ──────────────────────────────────────────────

#[test]
fn python_functools_partial_marked_callback() {
    let src = r#"
import functools

def make_adder(n):
    add = functools.partial(lambda x, y: x + y, n)
    return add
"#;
    let g = parse_py(src);
    // `functools.partial(...)` → FLAG_CALLBACK.
    let meta = g.call_metas.iter().find(|m| m.caller_name == "make_adder");
    assert!(
        meta.is_some(),
        "expected RawCallMeta for functools.partial(); call_metas: {:?}",
        g.call_metas
    );
    let meta = meta.unwrap();
    assert_eq!(
        meta.flags & CallMeta::FLAG_CALLBACK,
        CallMeta::FLAG_CALLBACK,
        "functools.partial() must set FLAG_CALLBACK"
    );
    assert!(
        meta.dispatch_type.contains("partial"),
        "dispatch_type should mention 'partial', got: {:?}",
        meta.dispatch_type
    );
}

// ── Python: callback called via method on param object ─────────────────────

#[test]
fn python_callback_method_call_marked_dynamic() {
    let src = r#"
def notify_all(handler, event):
    handler.on_event(event)
"#;
    let g = parse_py(src);
    // `handler.on_event(event)` where `handler` is a param → FLAG_DYNAMIC_DISPATCH.
    let meta = g.call_metas.iter().find(|m| m.caller_name == "notify_all");
    assert!(
        meta.is_some(),
        "expected RawCallMeta for method call on param handler; call_metas: {:?}",
        g.call_metas
    );
    let meta = meta.unwrap();
    assert_eq!(
        meta.flags & CallMeta::FLAG_DYNAMIC_DISPATCH,
        CallMeta::FLAG_DYNAMIC_DISPATCH,
        "handler.on_event() through param must set FLAG_DYNAMIC_DISPATCH"
    );
}
