use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CSharpProvider::new().expect("provider");
    p.parse_file(Path::new("test.cs"), src.as_bytes())
        .expect("parse")
}

fn enums(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Enum)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn simple_enum() {
    let src = r#"enum E { A, B }"#;
    let g = parse(src);
    let e = enums(&g);
    assert_eq!(e.len(), 1, "expected 1 Enum, got {e:?}");
    assert_eq!(e[0], "E");
}

#[test]
fn flags_enum_with_attribute() {
    let src = r#"[Flags] enum E { A = 1, B = 2 }"#;
    let g = parse(src);
    let e = enums(&g);
    assert_eq!(e.len(), 1, "expected 1 Enum, got {e:?}");
    assert_eq!(e[0], "E");
}

#[test]
fn public_enum() {
    let src = r#"public enum Day { Mon, Tue }"#;
    let g = parse(src);
    let e = enums(&g);
    assert_eq!(e.len(), 1, "expected 1 Enum, got {e:?}");
    assert_eq!(e[0], "Day");
}

#[test]
fn enum_not_emitted_as_class() {
    // Enum must be NodeKind::Enum, not Class (was previously mapped to @class).
    let src = r#"public enum Status { Active, Inactive }"#;
    let g = parse(src);
    let cls: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Class)
        .collect();
    assert!(
        cls.is_empty(),
        "enum was incorrectly emitted as Class: {cls:?}"
    );
    assert_eq!(enums(&g).len(), 1);
}

#[test]
fn multiple_enums_in_file() {
    let src = r#"
        public enum Color { Red, Green, Blue }
        internal enum Status { On, Off }
    "#;
    let g = parse(src);
    let mut e = enums(&g);
    e.sort_unstable();
    assert_eq!(e.len(), 2, "expected 2 Enums, got {e:?}");
    assert!(e.contains(&"Color"), "{e:?}");
    assert!(e.contains(&"Status"), "{e:?}");
}
