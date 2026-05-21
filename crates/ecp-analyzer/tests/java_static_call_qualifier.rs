use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = JavaProvider::new().expect("provider");
    p.parse_file(Path::new("Test.java"), src.as_bytes())
        .expect("parse")
}

/// When `locals.lookup` misses (identifier is a static class name, not a typed
/// local variable), the callee must be emitted as `ClassName.methodName` rather
/// than bare `methodName`.  Regression for the bug where `Util.helper(x)` was
/// recorded as `helper`, preventing Tier 2.5 qualifier-scoped resolution.
#[test]
fn java_static_call_emits_qualified_callee() {
    let src = r#"
class Util {
    static String helper(String s) { return s; }
}
class Caller {
    void m() { Util.helper("x"); }
}
"#;
    let graph = parse(src);

    let caller_m = graph
        .nodes
        .iter()
        .find(|n| n.name == "m" && n.kind == NodeKind::Method)
        .unwrap_or_else(|| {
            panic!(
                "Caller.m must be emitted; nodes: {:?}",
                graph
                    .nodes
                    .iter()
                    .map(|n| (&n.name, &n.kind))
                    .collect::<Vec<_>>()
            )
        });

    assert!(
        caller_m.calls.iter().any(|c| c == "Util.helper"),
        "Caller.m must record 'Util.helper' in its calls list; got: {:?}",
        caller_m.calls
    );
}

/// Counterpart: a typed local variable call still uses the declared type, not
/// the variable name.  E.g. `Foo f = new Foo(); f.bar()` → `Foo.bar`.
#[test]
fn java_typed_local_call_uses_declared_type() {
    let src = r#"
class Foo {
    void bar() {}
}
class Client {
    void run() {
        Foo f = new Foo();
        f.bar();
    }
}
"#;
    let graph = parse(src);

    let run = graph
        .nodes
        .iter()
        .find(|n| n.name == "run" && n.kind == NodeKind::Method)
        .unwrap_or_else(|| panic!("Client.run must be emitted"));

    assert!(
        run.calls.iter().any(|c| c == "Foo.bar"),
        "Client.run must record 'Foo.bar' (type-resolved); got: {:?}",
        run.calls
    );
    assert!(
        !run.calls.iter().any(|c| c == "f.bar"),
        "variable name 'f' must not appear in calls; got: {:?}",
        run.calls
    );
}
