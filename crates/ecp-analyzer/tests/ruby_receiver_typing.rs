//! Receiver typing, e2e: Ruby has no declared types; the binder types the
//! inline constructor chain `Derived.new.greet`. The callee's method is
//! inherited from a base type in another file, and an unrelated decoy type
//! declares a method of the same name. The ladder must resolve the typed callee
//! through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::ruby::parser::RubyProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_constructor_chain_inherited_method_resolves_to_base_class() {
    let provider = RubyProvider::new().expect("RubyProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "lib/base.rb",
                r#"class Base
  def greet
    1
  end
end
"#,
            ),
            (
                "lib/derived.rb",
                r#"require_relative 'base'

class Derived < Base
end
"#,
            ),
            (
                "lib/decoy.rb",
                r#"class Decoy
  def greet
    2
  end
end
"#,
            ),
            (
                "lib/app.rb",
                r#"require_relative 'derived'

def run
  Derived.new.greet
end
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "lib/base.rb");
}
