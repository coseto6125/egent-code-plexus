//! `Overrides` in Dart binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! The parser keeps `@override` in the method's decorators, so the marked
//! method pairs with a same-file base as `Exact`.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_dart_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::dart::parser::DartProvider::new().unwrap(),
        ("a/decoy.dart", "class Base {\n  void run() {}\n}\n"),
        ("b/child.dart", "class Base {\n  void run() {}\n}\nclass Child extends Base {\n  @override\n  void run() {}\n}\n"),
        "run",
        // Contract: a marked override into a same-file base is Exact.
        Evidence::Exact,
    );
}

fn dart() -> ecp_analyzer::dart::parser::DartProvider {
    ecp_analyzer::dart::parser::DartProvider::new().unwrap()
}

const MIXIN_FIXTURE: &str = "mixin M {\n  void m() {}\n}\nclass B {\n  void m() {}\n}\nclass I {\n  void m() {}\n}\nclass C extends B with M implements I {\n  @override\n  void m() {}\n}\n";

/// Stands for: the Dart parser lists `with` mixins between the superclass
/// and the interfaces, the order the `Overrides` lookup reads.
#[test]
fn test_dart_parser_mixins_listed_between_superclass_and_interfaces() {
    use ecp_core::analyzer::provider::LanguageProvider;
    let lg = dart()
        .parse_file(std::path::Path::new("c.dart"), MIXIN_FIXTURE.as_bytes())
        .expect("parse");
    let c = lg.nodes.iter().find(|n| n.name == "C").expect("class C");
    assert_eq!(c.heritage, ["B", "M", "I"]);
}

/// Stands for: Dart looks a method up through the mixins, last first, then
/// the superclass, so `C.m` binds `M.m`, never `B.m`; the interface `I` is
/// its own branch. The parser keeps the `@override` annotation on the
/// method, so the `Inferred` rule gives the marked class.
#[test]
fn test_overrides_dart_mixin_lookup_selects_target() {
    let graph = overrides_support::graph_of(&dart(), &[("c.dart", MIXIN_FIXTURE)]);
    assert_eq!(
        overrides_support::override_rows(&graph),
        [(10, 1), (10, 7)],
        "C.m -> M.m and C.m -> I.m; never B.m (row 4)"
    );
    for edge in overrides_support::overrides(&graph) {
        // Contract: `@override` marks the method, so each edge is Exact.
        overrides_support::assert_evidence(&edge, Evidence::Exact);
    }
}
