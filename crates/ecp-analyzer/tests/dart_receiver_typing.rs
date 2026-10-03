//! Receiver typing, e2e: the binder types a method's formal parameter (`Derived
//! d`). The callee's method is inherited from a base type in another file, and
//! an unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::dart::parser::DartProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

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
