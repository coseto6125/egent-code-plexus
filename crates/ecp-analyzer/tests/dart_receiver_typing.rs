//! Receiver typing, e2e: the binder types a method's formal parameter (`Derived
//! d`). The callee's method is inherited from a base type in another file, and
//! an unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

use crate::name_ref_support::graph;
use crate::receiver_typing_support;

use ecp_analyzer::dart::parser::DartProvider;
use receiver_typing_support::{assert_single_call_into_base, call_rows_to, parse_all};

#[test]
fn test_typed_param_inherited_method_resolves_to_base_class() {
    let provider = DartProvider::new().expect("DartProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "lib/base.dart",
                r#"class Base {
  int greet() {
    return 1;
  }
}
"#,
            ),
            (
                "lib/derived.dart",
                r#"import 'base.dart';

class Derived extends Base {}
"#,
            ),
            (
                "lib/decoy.dart",
                r#"class Decoy {
  int greet() {
    return 2;
  }
}
"#,
            ),
            (
                "lib/app.dart",
                r#"import 'derived.dart';

class App {
  int run(Derived d) {
    return d.greet();
  }
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "lib/base.dart");
}

/// Rule U: in Dart, a member call on an untyped receiver that binds a method by
/// its name alone is a guess: 0.55, `call:untyped-member`, from any tier.
/// The caller's own type and an ambiguous name bind nothing; the nearest
/// copy of a duplicated declaration keeps 0.6.
#[test]
fn test_untyped_member_call_binds_by_name_as_tagged_guess() {
    receiver_typing_support::assert_untyped_member_is_guess("dart");
}

/// Stands for: the block-scope rule holds for Dart as it does in the sibling
/// languages (`bound_method_reference.rs`). `A` and `B` both declare `m`; only
/// the second block calls, on its own `B obj`, so `obj.m()` binds `B.m`. The
/// one-block case is the positive control.
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
