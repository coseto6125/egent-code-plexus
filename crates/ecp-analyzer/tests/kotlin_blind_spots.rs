use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_kt(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = KotlinProvider::new().expect("KotlinProvider::new");
    provider
        .parse_file(Path::new("test.kt"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── Class.forName via Java reflection bridge ──

#[test]
fn kotlin_class_for_name_emits_blind_spot() {
    let src = r#"
fun load(name: String) {
    val c = Class.forName(name)
}
"#;
    let g = parse_kt(src);
    assert!(
        kinds(&g).contains(&"kt-class-forname"),
        "expected kt-class-forname; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn kotlin_class_for_name_literal_still_emits_blind_spot() {
    let src = r#"
fun load() {
    val c = Class.forName("com.example.Plugin")
}
"#;
    let g = parse_kt(src);
    assert!(
        kinds(&g).contains(&"kt-class-forname"),
        "literal arg still emits (class body opaque); got: {:?}",
        kinds(&g)
    );
}

// ── Method.invoke via reflection bridge ──

#[test]
fn kotlin_method_invoke_emits_blind_spot() {
    let src = r#"
import java.lang.reflect.Method

fun run(m: Method, target: Any, args: Array<Any>) {
    m.invoke(target, args)
}
"#;
    let g = parse_kt(src);
    assert!(
        kinds(&g).contains(&"kt-method-invoke"),
        "expected kt-method-invoke; got: {:?}",
        kinds(&g)
    );
}

#[test]
fn kotlin_chained_reflective_call_emits_invoke() {
    let src = r#"
fun chain(name: String, m: String, target: Any) {
    Class.forName(name).getDeclaredMethod(m).invoke(target)
}
"#;
    let g = parse_kt(src);
    let ks = kinds(&g);
    assert!(
        ks.contains(&"kt-method-invoke"),
        "chain must emit kt-method-invoke; got: {:?}",
        ks
    );
    assert!(
        ks.contains(&"kt-class-forname"),
        "chain must also emit kt-class-forname; got: {:?}",
        ks
    );
}

// ── unrelated: NOT blind ──

#[test]
fn kotlin_interface_method_call_emits_no_blind_spot() {
    let src = r#"
interface Handler { fun handle() }
fun run(h: Handler) { h.handle() }
"#;
    let g = parse_kt(src);
    assert!(
        g.blind_spots.is_empty(),
        "interface dispatch must not emit BlindSpot; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn kotlin_ordinary_call_emits_no_blind_spot() {
    let src = "fun add(a: Int, b: Int) = a + b\nfun main() { val x = add(1, 2) }";
    let g = parse_kt(src);
    assert!(
        g.blind_spots.is_empty(),
        "ordinary call must not emit; got: {:?}",
        g.blind_spots
    );
}
