use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_java(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = JavaProvider::new().expect("JavaProvider::new");
    provider
        .parse_file(Path::new("Test.java"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── Class.forName: runtime class load ──

#[test]
fn java_class_for_name_with_variable_emits_blind_spot() {
    let src = r#"
class Loader {
    void load(String name) throws Exception {
        Class<?> c = Class.forName(name);
    }
}
"#;
    let g = parse_java(src);
    assert!(
        kinds(&g).contains(&"java-class-forname"),
        "expected java-class-forname; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn java_class_for_name_literal_still_emits_blind_spot() {
    // Even a literal arg is blind: the class body is opaque to ecp; the
    // LLM should know the load happened.
    let src = r#"
class Loader {
    void load() throws Exception {
        Class<?> c = Class.forName("com.example.Plugin");
    }
}
"#;
    let g = parse_java(src);
    assert!(
        kinds(&g).contains(&"java-class-forname"),
        "expected java-class-forname for literal arg; got: {:?}",
        kinds(&g)
    );
}

// ── Method.invoke / reflective invocation ──

#[test]
fn java_method_invoke_emits_blind_spot() {
    let src = r#"
import java.lang.reflect.Method;

class Dispatcher {
    void run(Method m, Object target, Object[] args) throws Exception {
        m.invoke(target, args);
    }
}
"#;
    let g = parse_java(src);
    assert!(
        kinds(&g).contains(&"java-method-invoke"),
        "expected java-method-invoke; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn java_chained_reflective_call_emits_blind_spot_at_invoke() {
    // Class.forName(name).getDeclaredMethod(m).invoke(target, args) — the
    // outermost call (invoke) is the dispatch site.
    let src = r#"
class Chain {
    void run(String name, String m, Object target) throws Exception {
        Class.forName(name).getDeclaredMethod(m).invoke(target);
    }
}
"#;
    let g = parse_java(src);
    let ks = kinds(&g);
    assert!(
        ks.contains(&"java-method-invoke"),
        "chained reflective call must emit java-method-invoke; got: {:?}",
        ks
    );
    assert!(
        ks.contains(&"java-class-forname"),
        "Class.forName in chain also emits its own anchor; got: {:?}",
        ks
    );
}

// ── unrelated: NOT blind ──

#[test]
fn java_ordinary_call_emits_no_blind_spot() {
    let src = r#"
class A {
    int add(int a, int b) { return a + b; }
    void main() { int x = add(1, 2); }
}
"#;
    let g = parse_java(src);
    assert!(
        g.blind_spots.is_empty(),
        "ordinary method call must not emit; got: {:?}",
        g.blind_spots
    );
}

// ── span shape ──

#[test]
fn java_class_for_name_span_single_line() {
    let src = "class A { void x(String n) throws Exception { Class.forName(n); } }";
    let g = parse_java(src);
    let bs = g
        .blind_spots
        .iter()
        .find(|b| b.kind == "java-class-forname")
        .expect("java-class-forname BlindSpot");
    let (sr, _, er, _) = bs.span;
    assert_eq!(sr, er, "single-line forName must keep start_row == end_row");
}
