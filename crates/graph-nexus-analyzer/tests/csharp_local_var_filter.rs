use graph_nexus_analyzer::c_sharp::parser::CSharpProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CSharpProvider::new().expect("provider");
    p.parse_file(Path::new("test.cs"), src.as_bytes()).expect("parse")
}

fn variables(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Variable)
        .map(|n| n.name.as_str())
        .collect()
}

fn properties(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property)
        .map(|n| n.name.as_str())
        .collect()
}

/// Method-local `int x = 0;` must NOT appear as Variable.
#[test]
fn local_declaration_not_emitted_as_variable() {
    let src = r#"
        class C {
            void M() {
                int localVar = 0;
                string s = "hello";
            }
        }
    "#;
    let g = parse(src);
    let vars = variables(&g);
    assert!(
        vars.is_empty(),
        "method-local declarations emitted as Variable: {vars:?}"
    );
}

/// Method parameters must NOT appear as Variable.
#[test]
fn parameter_not_emitted_as_variable() {
    let src = r#"
        class C {
            void M(int param, string name) {}
        }
    "#;
    let g = parse(src);
    let vars = variables(&g);
    assert!(
        vars.is_empty(),
        "parameters emitted as Variable: {vars:?}"
    );
}

/// Class fields stay as Property; locals in the same class must not leak.
#[test]
fn class_field_stays_property_locals_absent() {
    let src = r#"
        class Service {
            private int _count;
            public string Name { get; set; }

            void Run() {
                int temp = 42;
                var builder = new StringBuilder();
            }
        }
    "#;
    let g = parse(src);
    let vars = variables(&g);
    assert!(
        vars.is_empty(),
        "locals/params leaked as Variable: {vars:?}"
    );
    let props = properties(&g);
    assert!(
        props.contains(&"_count"),
        "_count missing from Property: {props:?}"
    );
    assert!(
        props.contains(&"Name"),
        "Name missing from Property: {props:?}"
    );
}

/// `using var` inside a method must not emit a Variable node.
#[test]
fn using_var_not_emitted_as_variable() {
    let src = r#"
        class C {
            void M() {
                using var conn = new SqlConnection();
            }
        }
    "#;
    let g = parse(src);
    let vars = variables(&g);
    assert!(
        vars.is_empty(),
        "using-var emitted as Variable: {vars:?}"
    );
}

/// `foreach var` loop variable must not emit a Variable node.
#[test]
fn foreach_var_not_emitted_as_variable() {
    let src = r#"
        class C {
            void M() {
                foreach (var item in list) { }
            }
        }
    "#;
    let g = parse(src);
    let vars = variables(&g);
    assert!(
        vars.is_empty(),
        "foreach-var emitted as Variable: {vars:?}"
    );
}
