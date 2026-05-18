use graph_nexus_cli::commands::group::extractors::http_go::extract_http;
use graph_nexus_cli::commands::group::types::{ContractRole, ContractType};
use std::path::Path;

#[test]
fn go_net_http_handle_func_extracts_routes() {
    let path = Path::new("tests/fixtures/group/go/http_server.go");
    let source = std::fs::read(path).unwrap();
    let contracts = extract_http(path, &source);

    let ids: Vec<&str> = contracts.iter().map(|c| c.contract_id.as_str()).collect();
    assert!(ids.contains(&"http:ANY:/api/users"),
            "missing /api/users; got {ids:?}");
    assert!(ids.contains(&"http:ANY:/api/users/{id}"),
            "missing /api/users/{{id}}; got {ids:?}");

    for c in &contracts {
        assert_eq!(c.contract_type, ContractType::Http);
        assert_eq!(c.role, ContractRole::Provider);
        assert_eq!(c.confidence, 0.85);
    }
}

#[test]
fn go_non_route_calls_ignored() {
    let source = b"package main\nfunc main() { println(\"hi\") }\n";
    let contracts = extract_http(Path::new("x.go"), source);
    assert!(contracts.is_empty());
}
