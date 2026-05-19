use cgn_cli::commands::group::extractors::grpc_rust::extract_grpc;
use cgn_cli::commands::group::types::{ContractRole, ContractType};
use std::path::Path;

#[test]
fn rust_grpc_server_registration_extracts_service() {
    let path = Path::new("tests/fixtures/group/rust/grpc_server.rs");
    let source = std::fs::read(path).unwrap();
    let contracts = extract_grpc(path, &source);
    let ids: Vec<&str> = contracts.iter().map(|c| c.contract_id.as_str()).collect();
    assert!(ids.contains(&"grpc:UserService:*"), "got {ids:?}");
    assert_eq!(contracts[0].contract_type, ContractType::Grpc);
    assert_eq!(contracts[0].role, ContractRole::Provider);
    assert_eq!(contracts[0].confidence, 0.9);
}

#[test]
fn rust_non_grpc_calls_ignored() {
    let source = b"fn main() {}\n";
    let contracts = extract_grpc(Path::new("x.rs"), source);
    assert!(contracts.is_empty());
}
