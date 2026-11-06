//! Stands for: the block-scope rule holds for Dart as it does in the sibling
//! languages (`bound_method_reference.rs`). `A` and `B` both declare `m`; only
//! the second block calls, on its own `B obj`, so `obj.m()` binds `B.m`. The
//! one-block case is the positive control.

use crate::name_ref_support::graph;
use crate::receiver_typing_support::call_rows_to;

use ecp_analyzer::dart::parser::DartProvider;

#[test]
fn test_calls_dart_block_scoped_same_name_locals_bind_second_block_type() {
    let provider = DartProvider::new().unwrap();
    let header = "import 'a.dart';\nimport 'b.dart';\nvoid run() {\n";
    for (scenario, blocks) in [
        (
            "two blocks",
            "  { A obj = A(); }\n  { B obj = B(); obj.m(); }\n",
        ),
        ("one block", "  { B obj = B(); obj.m(); }\n"),
    ] {
        let used = format!("{header}{blocks}}}\n");
        let g = graph(
            &provider,
            &[
                ("a.dart", "class A {\n  void m() {}\n}\n"),
                ("b.dart", "class B {\n  void m() {}\n}\n"),
                ("use.dart", used.as_str()),
            ],
        );
        let files: Vec<String> = call_rows_to(&g, "run", "m")
            .into_iter()
            .map(|row| row.0)
            .collect();
        assert_eq!(files, ["b.dart"], "dart {scenario}");
    }
}

/// Stands for: a for-in variable, a catch variable and a closure parameter
/// hide a same-named outer local, so `obj.m()` inside them never takes the
/// outer `A`. Untyped there, the call has two `m` owners and binds nothing.
#[test]
fn test_calls_dart_loop_catch_and_closure_bindings_hide_outer_local_type() {
    let provider = DartProvider::new().unwrap();
    let header = "import 'a.dart';\nimport 'b.dart';\nvoid run(List bs) {\n  A obj = A();\n";
    for (scenario, body) in [
        ("for-in", "  for (final obj in bs) { obj.m(); }\n"),
        ("catch", "  try { } catch (obj) { obj.m(); }\n"),
        ("closure", "  bs.forEach((obj) { obj.m(); });\n"),
    ] {
        let used = format!("{header}{body}}}\n");
        let g = graph(
            &provider,
            &[
                ("a.dart", "class A {\n  void m() {}\n}\n"),
                ("b.dart", "class B {\n  void m() {}\n}\n"),
                ("use.dart", used.as_str()),
            ],
        );
        let files: Vec<String> = call_rows_to(&g, "run", "m")
            .into_iter()
            .map(|row| row.0)
            .collect();
        assert!(
            !files.contains(&"a.dart".to_string()),
            "dart {scenario}: {files:?}"
        );
    }
}
