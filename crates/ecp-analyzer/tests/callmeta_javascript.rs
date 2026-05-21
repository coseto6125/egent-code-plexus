use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::CallMeta;
use std::path::Path;

fn parse_js(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    provider
        .parse_file(Path::new("test.js"), src.as_bytes())
        .expect("parse_file")
}

fn parse_ts(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    provider
        .parse_file(Path::new("test.ts"), src.as_bytes())
        .expect("parse_file")
}

// ── JS: callback parameter invoked ─────────────────────────────────────────

#[test]
fn js_callback_param_call_marked_callback() {
    let src = r#"
function process(items, callback) {
    for (const item of items) {
        callback(item);
    }
}
"#;
    let g = parse_js(src);
    // `callback(item)` — `callback` is a parameter, so FLAG_CALLBACK.
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
        "callback param call must NOT set FLAG_DIRECT"
    );
}

// ── JS: Function.prototype.call dispatch ─────────────────────────────────

#[test]
fn js_function_prototype_call_marked_dynamic() {
    let src = r#"
function invoke(fn, ctx, arg) {
    fn.call(ctx, arg);
}
"#;
    let g = parse_js(src);
    // `fn.call(ctx, arg)` — FLAG_CALLBACK | FLAG_DYNAMIC_DISPATCH.
    let meta = g.call_metas.iter().find(|m| m.caller_name == "invoke");
    assert!(
        meta.is_some(),
        "expected RawCallMeta for fn.call() in `invoke`; call_metas: {:?}",
        g.call_metas
    );
    let meta = meta.unwrap();
    assert_eq!(
        meta.flags & CallMeta::FLAG_CALLBACK,
        CallMeta::FLAG_CALLBACK,
        "fn.call() must set FLAG_CALLBACK"
    );
    assert_eq!(
        meta.flags & CallMeta::FLAG_DYNAMIC_DISPATCH,
        CallMeta::FLAG_DYNAMIC_DISPATCH,
        "fn.call() must set FLAG_DYNAMIC_DISPATCH"
    );
    assert!(
        meta.dispatch_type.contains("call"),
        "dispatch_type should mention 'call', got: {:?}",
        meta.dispatch_type
    );
}

// ── JS: direct function call — no CallMeta ─────────────────────────────────

#[test]
fn js_direct_call_no_callmeta() {
    let src = r#"
function helper(x) { return x + 1; }

function main() {
    helper(42);
}
"#;
    let g = parse_js(src);
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

// ── JS: Function.prototype.apply ─────────────────────────────────────────

#[test]
fn js_function_prototype_apply_marked_dynamic() {
    let src = r#"
function forward(fn, args) {
    fn.apply(null, args);
}
"#;
    let g = parse_js(src);
    let meta = g.call_metas.iter().find(|m| m.caller_name == "forward");
    assert!(
        meta.is_some(),
        "expected RawCallMeta for fn.apply() in `forward`; call_metas: {:?}",
        g.call_metas
    );
    let meta = meta.unwrap();
    assert!(
        meta.dispatch_type.contains("apply"),
        "dispatch_type should mention 'apply', got: {:?}",
        meta.dispatch_type
    );
}

// ── TS: callback param (typed as Function) ─────────────────────────────────

#[test]
fn ts_callback_param_call_marked_callback() {
    let src = r#"
function run(handler: (x: number) => void, value: number): void {
    handler(value);
}
"#;
    let g = parse_ts(src);
    // `handler(value)` — handler is a parameter → FLAG_CALLBACK.
    let meta = g.call_metas.iter().find(|m| m.caller_name == "run");
    assert!(
        meta.is_some(),
        "expected RawCallMeta for callback param in `run`; call_metas: {:?}",
        g.call_metas
    );
    let meta = meta.unwrap();
    assert_eq!(
        meta.flags & CallMeta::FLAG_CALLBACK,
        CallMeta::FLAG_CALLBACK,
        "typed callback param must set FLAG_CALLBACK"
    );
}
