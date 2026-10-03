//! Receiver typing, e2e: the binder types a parameter annotation (`d:
//! Derived`). The callee's method is inherited from a base type in another
//! file, and an unrelated decoy type declares a method of the same name. The
//! ladder must resolve the typed callee through the type's declared heritage to
//! the base.

mod receiver_typing_support;

use ecp_analyzer::typescript::parser::TypeScriptProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_typed_param_inherited_method_resolves_to_base_class() {
    let provider = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/base.ts",
                r#"export class Base {
  greet(): number {
    return 1;
  }
}
"#,
            ),
            (
                "src/derived.ts",
                r#"import { Base } from './base';

export class Derived extends Base {}
"#,
            ),
            (
                "src/decoy.ts",
                r#"export class Decoy {
  greet(): number {
    return 2;
  }
}
"#,
            ),
            (
                "src/app.ts",
                r#"import { Derived } from './derived';

export function run(d: Derived): number {
  return d.greet();
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "src/base.ts");
}
