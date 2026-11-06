//! Receiver typing, e2e: JavaScript has no declared types; the binder types
//! `this` as the enclosing class. The callee's method is inherited from a base
//! type in another file, and an unrelated decoy type declares a method of the
//! same name. The ladder must resolve the typed callee through the type's
//! declared heritage to the base.

use crate::receiver_typing_support;

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_this_receiver_inherited_method_resolves_to_base_class() {
    let provider = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/base.js",
                r#"export class Base {
  greet() {
    return 1;
  }
}
"#,
            ),
            (
                "src/derived.js",
                r#"import { Base } from './base';

export class Derived extends Base {
  run() {
    return this.greet();
  }
}
"#,
            ),
            (
                "src/decoy.js",
                r#"export class Decoy {
  greet() {
    return 2;
  }
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "src/base.js");
}

/// Stands for: JavaScript is a guess language (decision A09): a member call on an
/// untyped receiver binds a method by its name alone as the tagged guess
/// 0.55 `call:untyped-member`, from any tier. The caller's own type and an
/// ambiguous name bind nothing; the nearest copy keeps 0.6.
#[test]
fn test_untyped_member_call_binds_by_name_as_tagged_guess() {
    receiver_typing_support::assert_untyped_member_is_guess("js");
}
