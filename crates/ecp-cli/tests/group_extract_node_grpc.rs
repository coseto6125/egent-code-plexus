use ecp_cli::commands::group::extractors::grpc_node::extract_grpc;
use ecp_cli::commands::group::types::{ContractRole, ContractType};
use std::path::Path;

#[test]
fn node_grpc_server_registration_extracts_service() {
    let path = Path::new("tests/fixtures/group/node/grpc_server.ts");
    let source = std::fs::read(path).unwrap();
    let contracts = extract_grpc(path, &source);
    let ids: Vec<&str> = contracts.iter().map(|c| c.contract_id.as_str()).collect();
    assert!(ids.contains(&"grpc:UserService:*"), "got {ids:?}");
    assert_eq!(contracts[0].contract_type, ContractType::Grpc);
    assert_eq!(contracts[0].role, ContractRole::Provider);
    assert_eq!(contracts[0].confidence, 0.9);
}

#[test]
fn node_non_grpc_calls_ignored() {
    let source = b"const x = 1;\n";
    let contracts = extract_grpc(Path::new("x.ts"), source);
    assert!(contracts.is_empty());
}

/// A registration inside a JSX attribute is found in a `.tsx` / `.jsx` file.
/// The fixture is a parse probe, not a realistic server: the TypeScript
/// grammar reads `<button onClick={..}>` as an ERROR and loses the call.
#[test]
fn node_grpc_registration_inside_jsx_is_extracted() {
    let src = "export function Page() {\n  return <button onClick={() => server.addService(user_proto.UserService.service, impl)}>Go</button>;\n}\n";
    for path in ["Page.tsx", "Page.jsx", "src/ui/Page.js"] {
        let ids: Vec<String> = extract_grpc(Path::new(path), src.as_bytes())
            .into_iter()
            .map(|c| c.contract_id)
            .collect();
        assert_eq!(ids, ["grpc:UserService:*"], "{path}");
    }
}

/// Guard: a `.ts` file with a type assertion still extracts; empty input
/// extracts nothing in either dialect.
#[test]
fn node_grpc_plain_ts_with_type_assertion_still_extracts() {
    let src = "const v = <string>raw;\nserver.addService(user_proto.UserService.service, impl);\n";
    let got = extract_grpc(Path::new("server.ts"), src.as_bytes());
    assert_eq!(got.len(), 1);
    assert!(extract_grpc(Path::new("empty.tsx"), b"").is_empty());
    assert!(extract_grpc(Path::new("empty.ts"), b"").is_empty());
}
