use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_dart(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = DartProvider::new().expect("DartProvider::new");
    provider
        .parse_file(Path::new("test.dart"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── Function.apply — reflective function invocation ──

#[test]
fn dart_function_apply_emits_blind_spot() {
    let src = r#"
void dispatch(Function f, List<dynamic> args) {
  Function.apply(f, args);
}
"#;
    let g = parse_dart(src);
    assert!(
        kinds(&g).contains(&"dart-function-apply"),
        "expected dart-function-apply; got: {:?}",
        kinds(&g)
    );
}

// ── dart:mirrors — runtime reflection ──

#[test]
fn dart_mirrors_import_emits_blind_spot() {
    // Importing dart:mirrors signals the file uses runtime reflection
    // even before any specific call. Emit at the import site.
    let src = r#"
import 'dart:mirrors';

void inspect(Object o) {
  var m = reflect(o);
}
"#;
    let g = parse_dart(src);
    assert!(
        kinds(&g).contains(&"dart-mirrors-import"),
        "expected dart-mirrors-import; got: {:?}",
        kinds(&g)
    );
}

// ── negative ──

#[test]
fn dart_ordinary_call_emits_no_blind_spot() {
    let src = "int add(int a, int b) => a + b;\nvoid main() { var x = add(1, 2); }";
    let g = parse_dart(src);
    assert!(
        g.blind_spots.is_empty(),
        "ordinary call must not emit; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn dart_unrelated_apply_method_skipped() {
    // `Function.apply` is precise — a different `.apply()` method must
    // not match.
    let src = r#"
class Builder {
  void apply() {}
}
void main() {
  Builder().apply();
}
"#;
    let g = parse_dart(src);
    assert!(
        !kinds(&g).contains(&"dart-function-apply"),
        "unrelated .apply() must not match; got: {:?}",
        kinds(&g)
    );
}
