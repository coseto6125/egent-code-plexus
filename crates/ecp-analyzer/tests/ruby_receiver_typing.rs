//! Receiver typing, e2e: Ruby has no declared types; the binder types the
//! inline constructor chain `Derived.new.greet`. The callee's method is
//! inherited from a base type in another file, and an unrelated decoy type
//! declares a method of the same name. The ladder must resolve the typed callee
//! through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::CallSite;
use receiver_typing_support::{
    assert_binder_emits, assert_single_call_into_base, build, callee_files, parse_all,
};
use std::path::Path;

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

/// Ruby reopens classes: `class Derived` in another file adds `greet` to the
/// same runtime class, which then overrides `Base#greet`. The caller sits in
/// the file declaring `Derived < Base`, so the type is unique there; the
/// ladder cannot tell the reopening from a different same-named class, so it
/// must not fall through to the base.
#[test]
fn test_reopened_class_member_blocks_inherited_method() {
    let provider = RubyProvider::new().expect("RubyProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "lib/base.rb",
                "class Base\n  def greet\n    1\n  end\nend\n",
            ),
            (
                "lib/model.rb",
                "class Derived < Base\nend\n\ndef run\n  Derived.new.greet\nend\n",
            ),
            (
                "lib/reopening.rb",
                "class Derived\n  def greet\n    3\n  end\nend\n",
            ),
            (
                "lib/decoy.rb",
                "class Decoy\n  def greet\n    2\n  end\nend\n",
            ),
        ],
    );
    assert_binder_emits(&graphs, "run", "Derived.greet");
    let targets = callee_files(&build(graphs), "run", "greet");
    assert!(
        !targets.contains(&"lib/base.rb".to_string()),
        "reopened Derived#greet overrides Base#greet: got {targets:?}"
    );
}

#[test]
fn test_receiver_type_long_preserving_chain_keeps_root_classification() {
    let provider = RubyProvider::new().unwrap();
    let length = 1000;
    for (root, expected, untyped) in [
        ("Item", "Item.save", false),
        ("Shop::Item", "save", false),
        ("self", "save", false),
        ("obj", "save", true),
        ("Item.pluck(:id)", "save", true),
    ] {
        let source = format!(
            "def caller\n  {root}{}.save\nend\n",
            ".where(:x)".repeat(length)
        );
        let local = provider
            .parse_file(Path::new("chain.rb"), source.as_bytes())
            .unwrap();
        let caller = local
            .nodes
            .iter()
            .find(|node| node.name == "caller")
            .unwrap();
        assert_eq!(
            caller.calls.len(),
            length + 1 + usize::from(root.contains("pluck"))
        );
        let outer = caller
            .calls
            .iter()
            .map(|raw| CallSite::parse(raw))
            .find(|site| site.fallback().name().ends_with("save"))
            .unwrap();
        assert_eq!(outer.fallback().name(), expected, "root={root}");
        assert_eq!(
            matches!(outer.fallback(), CallSite::UntypedMember(_)),
            untyped,
            "root={root}"
        );
    }
}

#[test]
fn test_receiver_type_self_chains_in_distinct_classes_keep_each_owner() {
    let provider = RubyProvider::new().unwrap();
    let source = "class First\n  def first\n    self.where(:x).save\n  end\nend\nclass Second\n  def second\n    self.where(:x).save\n  end\nend\n";
    let local = provider
        .parse_file(Path::new("owners.rb"), source.as_bytes())
        .unwrap();
    for (name, owner) in [("first", "First"), ("second", "Second")] {
        let caller = local.nodes.iter().find(|node| node.name == name).unwrap();
        assert!(
            caller.calls.contains(&format!("{owner}.save")),
            "{name}: {:?}",
            caller.calls
        );
        assert!(caller.calls.contains(&format!("{owner}.where")));
    }
}
