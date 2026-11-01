use ecp_cli::commands::group::extractors::http_node::extract_http;
use ecp_cli::commands::group::types::{ContractRole, ContractType};
use std::path::Path;

#[test]
fn node_express_extracts_routes() {
    let path = Path::new("tests/fixtures/group/node/http_server.ts");
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
fn node_non_route_calls_ignored() {
    let source = b"const x = 1;\n";
    let contracts = extract_http(Path::new("x.ts"), source);
    assert!(contracts.is_empty());
}

const JSX_ROUTES: &str = "export function Page() {\n  return (\n    <div>\n      <button onClick={() => fetch(\"/api/a\")}>Load</button>\n      <Form onSubmit={(d) => axios.post(\"/api/b\", d)} />\n      <i onClick={() => axios.get(\"/api/c\")}>it's</i>\n    </div>\n  );\n}\n";

fn sorted_ids(path: &str, source: &str) -> Vec<String> {
    let mut ids: Vec<String> = extract_http(Path::new(path), source.as_bytes())
        .into_iter()
        .map(|c| c.contract_id)
        .collect();
    ids.sort();
    ids
}

/// Client calls written inside JSX attributes (`onClick={() => fetch(..)}`)
/// are found in a `.tsx` / `.jsx` file. The TypeScript grammar reads the JSX
/// as an ERROR and loses the calls in it; the TSX grammar parses them.
#[test]
fn node_jsx_files_extract_clients_inside_jsx_attributes() {
    let want = ["http:GET:/api/a", "http:GET:/api/c", "http:POST:/api/b"];
    assert_eq!(sorted_ids("Page.tsx", JSX_ROUTES), want);
    assert_eq!(sorted_ids("Page.jsx", JSX_ROUTES), want);
}

/// Guard: a `.ts` file keeps the TypeScript grammar, where `<T>x` is a type
/// assertion, and still extracts its routes; empty input extracts nothing.
#[test]
fn node_plain_ts_with_type_assertion_still_extracts() {
    let src = "const v = <string>raw;\napp.get(\"/api/a\", ha);\n";
    assert_eq!(sorted_ids("server.ts", src), ["http:GET:/api/a"]);
    assert!(sorted_ids("empty.tsx", "").is_empty());
    assert!(sorted_ids("empty.ts", "").is_empty());
}
