//! Solidity "Named" dimension — alias / typedef detection.
//!
//! Emits `NodeKind::Typedef` for:
//!   - `using SafeMath for uint256;`  (using_directive with type_alias)
//!   - `type Currency is uint256;`    (user_defined_type_definition)
//!
//! Does NOT emit Typedef for:
//!   - `contract Foo {}` (stays Class)

use graph_nexus_analyzer::solidity::parser::SolidityProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<(String, NodeKind)> {
    let provider = SolidityProvider::new().expect("SolidityProvider::new");
    let graph = provider
        .parse_file(Path::new("t.sol"), src.as_bytes())
        .expect("parse_file");
    graph.nodes.iter().map(|n| (n.name.clone(), n.kind)).collect()
}

fn find_node<'a>(nodes: &'a [(String, NodeKind)], name: &str) -> &'a (String, NodeKind) {
    nodes
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("node `{name}` not found in {nodes:#?}"))
}

#[test]
fn test_solidity_using_directive_emits_typedef() {
    let src = "contract A {\n    using SafeMath for uint256;\n}\n";
    let nodes = parse(src);
    let n = find_node(&nodes, "SafeMath");
    assert_eq!(n.1, NodeKind::Typedef, "`using X for T` must emit NodeKind::Typedef");
}

#[test]
fn test_solidity_user_defined_type_emits_typedef() {
    let src = "type Currency is uint256;\n";
    let nodes = parse(src);
    let n = find_node(&nodes, "Currency");
    assert_eq!(n.1, NodeKind::Typedef, "`type X is T` must emit NodeKind::Typedef");
}

#[test]
fn test_solidity_contract_not_typedef() {
    let src = "contract Foo {}\n";
    let nodes = parse(src);
    let n = find_node(&nodes, "Foo");
    assert_eq!(n.1, NodeKind::Class, "contract must be NodeKind::Class, not Typedef");
    assert!(
        nodes.iter().all(|(_, k)| *k != NodeKind::Typedef),
        "contract must not emit any Typedef, got: {nodes:#?}"
    );
}

#[test]
fn test_solidity_both_typedef_forms_coexist() {
    let src = "type Price is uint256;\ncontract Market {\n    using SafeLib for uint256;\n}\n";
    let nodes = parse(src);
    let price = find_node(&nodes, "Price");
    assert_eq!(price.1, NodeKind::Typedef, "Price must be Typedef");
    let safe_lib = find_node(&nodes, "SafeLib");
    assert_eq!(safe_lib.1, NodeKind::Typedef, "SafeLib must be Typedef");
    let market = find_node(&nodes, "Market");
    assert_eq!(market.1, NodeKind::Class, "Market must be Class");
}

#[test]
fn test_solidity_interface_not_typedef() {
    let src = "interface IToken {}\n";
    let nodes = parse(src);
    let n = find_node(&nodes, "IToken");
    assert_eq!(n.1, NodeKind::Class, "interface must be NodeKind::Class");
}
