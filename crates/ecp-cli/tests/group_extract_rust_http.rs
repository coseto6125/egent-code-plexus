use ecp_cli::commands::group::extractors::http_rust::extract_http;
use ecp_cli::commands::group::types::{ContractRole, ContractType};
use std::path::Path;

#[test]
fn rust_axum_extracts_routes() {
    let path = Path::new("tests/fixtures/group/rust/http_server.rs");
    let source = std::fs::read(path).unwrap();
    let contracts = extract_http(path, &source);

    let ids: Vec<&str> = contracts.iter().map(|c| c.contract_id.as_str()).collect();
    assert!(
        ids.contains(&"http:POST:/api/users"),
        "missing POST /api/users; got {ids:?}"
    );
    assert!(
        ids.contains(&"http:GET:/api/users/:id"),
        "missing GET /api/users/:id; got {ids:?}"
    );

    for c in &contracts {
        assert_eq!(c.contract_type, ContractType::Http);
        assert_eq!(c.role, ContractRole::Provider);
        assert_eq!(c.confidence, 0.85);
    }
}

#[test]
fn rust_non_route_calls_ignored() {
    let source = b"fn main() {}\n";
    let contracts = extract_http(Path::new("x.rs"), source);
    assert!(contracts.is_empty());
}
