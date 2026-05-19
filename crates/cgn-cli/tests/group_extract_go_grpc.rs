use cgn_cli::commands::group::extractors::grpc_go::extract_grpc;
use cgn_cli::commands::group::types::{ContractRole, ContractType};
use std::path::Path;

#[test]
fn go_grpc_server_registration_extracts_service() {
    let path = Path::new("tests/fixtures/group/go/grpc_server.go");
    let source = std::fs::read(path).unwrap();
    let contracts = extract_grpc(path, &source);
    let ids: Vec<&str> = contracts.iter().map(|c| c.contract_id.as_str()).collect();
    assert!(ids.contains(&"grpc:UserService:*"), "got {ids:?}");
    assert_eq!(contracts[0].contract_type, ContractType::Grpc);
    assert_eq!(contracts[0].role, ContractRole::Provider);
    assert_eq!(contracts[0].confidence, 0.9);
}

#[test]
fn go_non_grpc_calls_ignored() {
    let source = b"package main\nfunc main() { println(\"hi\") }\n";
    let contracts = extract_grpc(Path::new("x.go"), source);
    assert!(contracts.is_empty());
}
