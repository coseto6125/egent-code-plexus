//! Ruby: no return annotations exist, so a method named for `self` leaves no `type_annotation` edge.
mod name_ref_support;

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use name_ref_support::{graph, refs};

/// `(target file, target name)` of every `type_annotation` edge out of `source`.
fn annotated(g: &ZeroCopyGraph, source: &str) -> Vec<(String, String)> {
    refs(g, source, RelType::Accesses, "type_annotation")
        .into_iter()
        .map(|(file, name, _, _)| (file, name))
        .collect()
}

/// Guard: a Ruby method that returns `self` (or `Self`, a constant) carries no
/// `type_annotation` edge to the owner or to a class named `Self`.
#[test]
fn test_method_returning_self_emits_no_annotation_edge() {
    let g = graph(
        &RubyProvider::new().unwrap(),
        &[(
            "lib/resolver.rb",
            "class Self\nend\n\nclass Resolver\n  def bump\n    self\n  end\n\n  def make\n    Self.new\n  end\nend\n",
        )],
    );
    assert_eq!(annotated(&g, "bump"), Vec::<(String, String)>::new());
    assert_eq!(annotated(&g, "make"), Vec::<(String, String)>::new());
}

/// Guard: Ruby has no type syntax, so a YARD `@return [Tok, nil]` comment and
/// a method returning `nil` or a `Tok` emit no `type_annotation` edge to the
/// class `Tok`, before and after the change.
#[test]
fn test_yard_nullable_return_emits_no_annotation_edge() {
    let g = graph(
        &RubyProvider::new().unwrap(),
        &[(
            "lib/app.rb",
            "class Tok\nend\n\nclass Owner\n  # @return [Tok, nil]\n  def find\n    nil\n  end\nend\n",
        )],
    );
    assert_eq!(annotated(&g, "find"), Vec::<(String, String)>::new());
}
