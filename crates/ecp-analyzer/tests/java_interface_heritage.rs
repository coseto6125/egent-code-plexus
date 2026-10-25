//! Java interface-to-interface inheritance reaches the graph as heritage.

use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

/// Stands for: `interface J extends I, K` lists both parents in J's
/// heritage, so `J → I` and `J → K` edges exist and Overrides can walk
/// through J. Failed before: tree-sitter-java 0.23 gives
/// `extends_interfaces` no field name, and the query matched it by one.
#[test]
fn test_parse_java_interface_extends_lists_every_parent_in_heritage() {
    let graph = JavaProvider::new()
        .unwrap()
        .parse_file(
            Path::new("J.java"),
            b"interface I {}\ninterface K {}\ninterface J extends I, K {\n    void run();\n}\n",
        )
        .unwrap();
    let j = graph
        .nodes
        .iter()
        .find(|n| n.name == "J" && n.kind == NodeKind::Interface)
        .expect("J is an Interface node");
    assert_eq!(j.heritage, ["I", "K"]);
}
