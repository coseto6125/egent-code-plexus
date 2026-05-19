//! Dart `extension X on T` and `extension type Foo(T x)` are type-level
//! declarations that add named members reachable through `T.fooMethod(...)`.
//! Previously gnx-rs's Dart parser had no capture for them — ref-gitnexus
//! emitted them as Class on Alamofire / bloc fixtures, producing 8 unpaired
//! ref_over Class entries. Map to NodeKind::Trait (closest semantic — extend
//! behaviour without subclassing) and rely on the aggregator's
//! {Interface, Struct, Enum, Annotation, Class, Trait} EQUIV class for parity.

use cgn_analyzer::dart::parser::DartProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::{LocalGraph, RawNode};
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = DartProvider::new().expect("DartProvider init");
    p.parse_file(Path::new("t.dart"), src.as_bytes()).expect("parse_file")
}

fn traits(g: &LocalGraph) -> Vec<&RawNode> {
    g.nodes.iter().filter(|n| n.kind == NodeKind::Trait).collect()
}

#[test]
fn extension_on_type_emits_trait() {
    let g = parse("extension AppLocalizationsX on BuildContext {\n}\n");
    let ts = traits(&g);
    assert_eq!(ts.len(), 1, "expected 1 Trait, got {:?}", g.nodes);
    assert_eq!(ts[0].name, "AppLocalizationsX");
}

#[test]
fn extension_with_body_emits_trait() {
    let g = parse(
        "extension SnakeCaseX on String {\n\
             String snakeCase() => this.toLowerCase();\n\
         }\n",
    );
    let ts = traits(&g);
    assert_eq!(ts.len(), 1);
    assert_eq!(ts[0].name, "SnakeCaseX");
}

// Note: `extension type Foo(...)` (Dart 3) uses an `extension_type_name`
// grammar node for the name, which the current query doesn't cover.
// Real-corpus samples in .sample_repo are all `extension X on T` form;
// adding extension_type support is a separate follow-up if it shows up
// in future fixtures.

#[test]
fn extension_does_not_double_emit_as_class() {
    let g = parse("extension PumpApp on WidgetTester {\n}\n");
    let classes: Vec<_> = g.nodes.iter().filter(|n| n.kind == NodeKind::Class).collect();
    assert!(
        classes.iter().all(|n| n.name != "PumpApp"),
        "extension leaked as Class: {:?}",
        classes,
    );
}

#[test]
fn mixin_still_emits_trait_after_extension_added() {
    // Regression: adding extension capture must not break the existing
    // mixin → Trait path.
    let g = parse("mixin M {\n}\n");
    let ts = traits(&g);
    assert_eq!(ts.len(), 1);
    assert_eq!(ts[0].name, "M");
}
