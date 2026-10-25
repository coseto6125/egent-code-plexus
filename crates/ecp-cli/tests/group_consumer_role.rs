//! Group extractors emit the `Consumer` side of a contract (architecture
//! direction, area 8). Before this change every extractor emitted only
//! `ContractRole::Provider`, so `matching.rs` had nothing to pair and every
//! cross-repo link list was empty.

use ecp_cli::commands::group::extractors::{
    grpc_java, grpc_rust, http_go, http_java, http_node, http_rust,
};
use ecp_cli::commands::group::matching::match_contracts;
use ecp_cli::commands::group::types::{
    ContractRole, ContractType, ExtractedContract, MatchType, StoredContract,
};
use ecp_core::config::GroupConfig;
use std::path::Path;
use tempfile::TempDir;

fn consumers(contracts: &[ExtractedContract]) -> Vec<&ExtractedContract> {
    contracts
        .iter()
        .filter(|c| c.role == ContractRole::Consumer)
        .collect()
}

fn consumer_ids(contracts: &[ExtractedContract]) -> Vec<&str> {
    consumers(contracts)
        .iter()
        .map(|c| c.contract_id.as_str())
        .collect()
}

fn provider_ids(contracts: &[ExtractedContract]) -> Vec<&str> {
    contracts
        .iter()
        .filter(|c| c.role == ContractRole::Provider)
        .map(|c| c.contract_id.as_str())
        .collect()
}

/// Stands for: a Node client call is a `Consumer` on the same `http:<VERB>:<path>`
/// id the Express route registers as `Provider`; an `axios.post(url, body)` call
/// has the shape of `app.post(path, handler)` and must not also register a route.
#[test]
fn test_group_extractors_emit_consumer_role_http_node() {
    let source = br#"
async function load(id: string) {
  await fetch("/api/users");
  await fetch("https://svc.internal/api/orders?page=1", { method: "POST" });
  await axios.get("/api/items", { params: { id } });
  await axios.post("/api/items", { id });
}
app.get("/api/health", healthHandler);
"#;
    let contracts = http_node::extract_http(Path::new("client.ts"), source);
    let ids = consumer_ids(&contracts);
    for want in [
        "http:GET:/api/users",
        "http:POST:/api/orders",
        "http:GET:/api/items",
        "http:POST:/api/items",
    ] {
        assert!(ids.contains(&want), "missing consumer {want}; got {ids:?}");
    }
    assert_eq!(provider_ids(&contracts), vec!["http:GET:/api/health"]);
    for c in consumers(&contracts) {
        assert_eq!(c.contract_type, ContractType::Http);
        assert!(c.meta.iter().any(|(k, v)| k == "line" && v != "0"));
    }
}

/// Stands for: two calls of one callee in one file keep distinct
/// `symbol_uid`s, because `matching.rs` tracks matched contracts by uid.
#[test]
fn test_group_extractors_emit_consumer_role_distinct_uid_per_call() {
    let source = b"fetch(\"/a\");\nfetch(\"/b\");\n";
    let contracts = http_node::extract_http(Path::new("client.ts"), source);
    let uids: Vec<&str> = consumers(&contracts)
        .iter()
        .map(|c| c.symbol_uid.as_str())
        .collect();
    assert_eq!(uids.len(), 2, "got {uids:?}");
    assert_ne!(uids[0], uids[1]);
}

/// Stands for: a non-literal URL gives no consumer rather than a guessed one.
#[test]
fn test_group_extractors_emit_consumer_role_dynamic_url_skipped() {
    let source =
        b"const u = base + \"/x\";\nfetch(u);\nfetch(`/api/${id}`);\nfetch(\"relative\");\n";
    let contracts = http_node::extract_http(Path::new("client.ts"), source);
    assert!(
        consumer_ids(&contracts).is_empty(),
        "got {:?}",
        consumer_ids(&contracts)
    );
}

#[test]
fn test_group_extractors_emit_consumer_role_http_java() {
    let source = br#"
class Client {
  void run() {
    restTemplate.getForObject("/api/users", String.class);
    webClient.post().uri("/api/orders").retrieve();
  }
}
"#;
    let contracts = http_java::extract_http(Path::new("Client.java"), source);
    let ids = consumer_ids(&contracts);
    assert!(ids.contains(&"http:GET:/api/users"), "got {ids:?}");
    assert!(ids.contains(&"http:POST:/api/orders"), "got {ids:?}");
}

#[test]
fn test_group_extractors_emit_consumer_role_http_go() {
    let source = br#"package main

import "net/http"

func run() {
	http.Get("http://svc/api/users?x=1")
	r.Get("/api/health", healthHandler)
}
"#;
    let contracts = http_go::extract_http(Path::new("client.go"), source);
    assert_eq!(consumer_ids(&contracts), vec!["http:GET:/api/users"]);
    assert_eq!(provider_ids(&contracts), vec!["http:GET:/api/health"]);
}

#[test]
fn test_group_extractors_emit_consumer_role_http_rust() {
    let source = br#"
async fn run(client: reqwest::Client) {
    client.get("/api/users").send().await;
    reqwest::get("https://svc/api/orders").await;
}
"#;
    let contracts = http_rust::extract_http(Path::new("client.rs"), source);
    let ids = consumer_ids(&contracts);
    assert!(ids.contains(&"http:GET:/api/users"), "got {ids:?}");
    assert!(ids.contains(&"http:GET:/api/orders"), "got {ids:?}");
}

#[test]
fn test_group_extractors_emit_consumer_role_grpc_java() {
    let source = br#"
class Client {
  void run(Channel channel) {
    var stub = GreeterGrpc.newBlockingStub(channel);
  }
}
"#;
    let contracts = grpc_java::extract_grpc(Path::new("Client.java"), source);
    assert_eq!(consumer_ids(&contracts), vec!["grpc:Greeter:*"]);
    assert_eq!(consumers(&contracts)[0].contract_type, ContractType::Grpc);
}

#[test]
fn test_group_extractors_emit_consumer_role_grpc_rust() {
    let source = br#"
use tonic::transport::Channel;
async fn run() {
    let c = GreeterClient::connect("http://svc:50051").await;
    let d = hello::greeter_client::GreeterClient::new(channel);
}
"#;
    let contracts = grpc_rust::extract_grpc(Path::new("client.rs"), source);
    assert_eq!(
        consumer_ids(&contracts),
        vec!["grpc:Greeter:*", "grpc:Greeter:*"]
    );
}

/// Stands for: `HttpClient::new()` in a file that never mentions tonic is not a
/// gRPC client.
#[test]
fn test_group_extractors_emit_consumer_role_grpc_rust_without_tonic_skipped() {
    let source = b"fn run() { let c = HttpClient::new(); }\n";
    let contracts = grpc_rust::extract_grpc(Path::new("client.rs"), source);
    assert!(contracts.is_empty());
}

fn stored(repo: &str, contracts: Vec<ExtractedContract>) -> Vec<StoredContract> {
    contracts
        .into_iter()
        .map(|inner| StoredContract {
            repo: repo.into(),
            inner,
        })
        .collect()
}

/// Stands for: the extractors feed `matching.rs`. A Provider in repo `api` and a
/// Consumer in repo `web` on the same route id make one exact cross-repo link,
/// from the consumer to the provider.
#[test]
fn test_group_matching_pairs_consumer_with_provider_across_repos() {
    let provider = http_node::extract_http(
        Path::new("server.ts"),
        b"app.get(\"/api/users\", listUsers);\n",
    );
    let consumer = http_node::extract_http(Path::new("client.ts"), b"fetch(\"/api/users\");\n");
    let mut all = stored("api", provider);
    all.extend(stored("web", consumer));
    let dir = TempDir::new().unwrap();
    let (links, unmatched) =
        match_contracts(&all, dir.path(), &GroupConfig::default(), true).unwrap();
    assert_eq!(links.len(), 1, "links: {links:?}");
    assert_eq!(links[0].from.repo, "web");
    assert_eq!(links[0].to.repo, "api");
    assert_eq!(links[0].contract_id, "http:GET:/api/users");
    assert_eq!(links[0].match_type, MatchType::Exact);
    assert!(unmatched.is_empty());
}

/// Stands for: a consumer in the same repo as the provider is not a
/// cross-repo link.
#[test]
fn test_group_matching_same_repo_consumer_not_linked() {
    let mut all = stored(
        "api",
        http_node::extract_http(Path::new("server.ts"), b"app.get(\"/api/users\", h);\n"),
    );
    all.extend(stored(
        "api",
        http_node::extract_http(Path::new("client.ts"), b"fetch(\"/api/users\");\n"),
    ));
    let dir = TempDir::new().unwrap();
    let (links, unmatched) =
        match_contracts(&all, dir.path(), &GroupConfig::default(), true).unwrap();
    assert!(links.is_empty());
    assert_eq!(unmatched.len(), 1);
}

/// Stands for: a provider that answers any verb (`app.all`) pairs with a
/// consumer of one verb on the same path, as a wildcard link.
#[test]
fn test_group_matching_any_verb_provider_pairs_with_verb_consumer() {
    let mut all = stored(
        "api",
        http_node::extract_http(Path::new("server.ts"), b"app.all(\"/api/users\", h);\n"),
    );
    all.extend(stored(
        "web",
        http_node::extract_http(Path::new("client.ts"), b"fetch(\"/api/users\");\n"),
    ));
    let dir = TempDir::new().unwrap();
    let (links, unmatched) =
        match_contracts(&all, dir.path(), &GroupConfig::default(), true).unwrap();
    assert_eq!(links.len(), 1, "links: {links:?}");
    assert_eq!(links[0].match_type, MatchType::Wildcard);
    assert!(unmatched.is_empty());
}
