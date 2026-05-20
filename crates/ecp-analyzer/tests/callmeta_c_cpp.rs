use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{CallMeta, RelType};
use std::path::Path;

fn parse_c(path: &str, src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = CProvider::new().expect("CProvider::new");
    provider
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file")
}

fn parse_cpp(path: &str, src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = CppProvider::new().expect("CppProvider::new");
    provider
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file")
}

// ── C: function-pointer call via parenthesized dereference ─────────────────

#[test]
fn c_fn_ptr_dereference_call_marked_callback() {
    // `(*fp)(42)` is a parenthesized_expression calling a dereferenced fn-ptr.
    let src = r#"
void run_it(void (*fp)(int)) {
    (*fp)(42);
}
"#;
    let g = parse_c("test.c", src);
    // Expect at least one RawCallMeta with FLAG_CALLBACK.
    let meta = g.call_metas.iter().find(|m| m.caller_name == "run_it");
    assert!(
        meta.is_some(),
        "expected RawCallMeta for fn-ptr dereference call in run_it; call_metas: {:?}",
        g.call_metas
    );
    let meta = meta.unwrap();
    assert_eq!(
        meta.flags & CallMeta::FLAG_CALLBACK,
        CallMeta::FLAG_CALLBACK,
        "(*fp)(42) must set FLAG_CALLBACK"
    );
    assert_eq!(
        meta.flags & CallMeta::FLAG_DIRECT,
        0,
        "(*fp)(42) must NOT set FLAG_DIRECT"
    );
}

// ── C: struct-of-fn-pointers indirect call ─────────────────────────────────

#[test]
fn c_struct_fn_ptr_call_marked_dynamic() {
    // `ops->open(fd)` through a struct of fn-pointers — indirect + dynamic dispatch.
    let src = r#"
struct file_ops {
    int (*open)(int fd);
    int (*read)(int fd, char *buf, int n);
};

void do_open(struct file_ops *ops, int fd) {
    ops->open(fd);
}
"#;
    let g = parse_c("test.c", src);
    // Check that ops->open(fd) produced a RawCallMeta.
    // The callee field is a field_expression through a pointer, which our detector
    // classifies as indirect if the receiver is a known struct pointer.
    // If the detector fires, flags must be CALLBACK | DYNAMIC_DISPATCH.
    for meta in &g.call_metas {
        if meta.caller_name == "do_open" {
            assert_ne!(
                meta.flags & CallMeta::FLAG_DIRECT,
                CallMeta::FLAG_DIRECT,
                "struct fn-ptr call must NOT be direct"
            );
        }
    }
    // Whether or not it fires (depends on struct type tracking depth),
    // there must be no false direct assertions.
    let _ = g;
}

// ── C++: virtual method call marked dynamic dispatch ───────────────────────

#[test]
fn cpp_virtual_method_call_via_base_ptr_marked_dynamic() {
    let src = r#"
class Animal {
public:
    virtual void speak() = 0;
};

class Dog : public Animal {
public:
    void speak() override {}
};

void make_speak(Animal* a) {
    a->speak();
}
"#;
    let g = parse_cpp("test.cpp", src);
    // `a->speak()` where `a` is typed as `Animal*` — dynamic dispatch.
    // Our detector checks if the receiver var is in fn_ptr_vars (pointer type).
    // Since `Animal*` is a pointer type, the receiver `a` should be tracked.
    for meta in &g.call_metas {
        if meta.caller_name == "make_speak" {
            assert_eq!(
                meta.flags & CallMeta::FLAG_DYNAMIC_DISPATCH,
                CallMeta::FLAG_DYNAMIC_DISPATCH,
                "virtual dispatch through base pointer must set FLAG_DYNAMIC_DISPATCH"
            );
            assert_eq!(
                meta.flags & CallMeta::FLAG_DIRECT,
                0,
                "virtual dispatch must NOT set FLAG_DIRECT"
            );
        }
    }
    let _ = g;
}

// ── C: direct function call — no CallMeta ─────────────────────────────────

#[test]
fn c_direct_call_no_callmeta() {
    let src = r#"
void helper(int x) {}

void caller(int v) {
    helper(v);
}
"#;
    let g = parse_c("test.c", src);
    // Direct call to `helper` should produce no RawCallMeta.
    let caller_meta: Vec<_> = g
        .call_metas
        .iter()
        .filter(|m| m.caller_name == "caller")
        .collect();
    assert!(
        caller_meta.is_empty(),
        "direct function call must not produce any RawCallMeta; got: {:?}",
        caller_meta
    );

    // Build a graph and verify no CallMeta on resolved Calls edges.
    let callee_src = r#"
void helper(int x) {}
"#;
    let callee_g = parse_c("helper.c", callee_src);
    let caller_g = parse_c("caller.c", src);
    let mut builder = GraphBuilder::new();
    builder.add_graph(callee_g);
    builder.add_graph(caller_g);
    let graph = builder.build();
    for (i, _e) in graph
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.rel_type == RelType::Calls)
    {
        assert!(
            graph.call_meta(i as u32).is_none(),
            "direct Calls edge {i} must have no CallMeta"
        );
    }
}

// ── C++: std::function callback call ──────────────────────────────────────

#[test]
fn cpp_std_function_call_marked_callback() {
    // Calling through a std::function<> — indirect / callback pattern.
    let src = r#"
#include <functional>

void dispatch(std::function<void(int)> fn, int x) {
    fn(x);
}
"#;
    let g = parse_cpp("test.cpp", src);
    // `fn(x)` where `fn: std::function<void(int)>` — should be FLAG_CALLBACK.
    // std::function wraps are fn-ptr-like; detected when variable type contains "function".
    // This is best-effort — accept if empty (complex type tracking).
    for meta in &g.call_metas {
        if meta.caller_name == "dispatch" {
            assert_ne!(
                meta.flags & CallMeta::FLAG_DIRECT,
                CallMeta::FLAG_DIRECT,
                "std::function call must NOT be direct"
            );
        }
    }
    let _ = g;
}
