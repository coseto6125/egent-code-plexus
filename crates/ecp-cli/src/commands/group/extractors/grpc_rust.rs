//! Rust gRPC server-registration extractor: .add_service(<Svc>Server::new(...)) via tonic.

use super::{capture_line, compile_client_query, consumer_contract};
use crate::commands::group::types::{ContractRole, ContractType, ExtractedContract, SymbolRef};
use std::path::Path;
use std::sync::LazyLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub(super) const SERVICE_CONFIDENCE: f32 = 0.9;

/// Matches `.add_service(<path>::<SvcServer>::new(...))`.
/// Captures the `add_service` field identifier and the `<SvcServer>` path segment
/// immediately before `::new`. The segment is the `name` of the scoped_identifier
/// whose own `name` is `new`.
const QUERY_SRC: &str = r#"
(call_expression
  function: (field_expression
    field: (field_identifier) @add_fn)
  arguments: (arguments
    (call_expression
      function: (scoped_identifier
        path: (scoped_identifier
          name: (identifier) @svc_server)
        name: (identifier) @new_fn))))
"#;

/// Client constructors: `GreeterClient::connect(addr)` and
/// `greeter_client::GreeterClient::new(channel)`.
const CLIENT_QUERY_SRC: &str = r#"
(call_expression
  function: (scoped_identifier
    path: [(identifier) @svc_client
           (scoped_identifier name: (identifier) @svc_client)]
    name: (identifier) @ctor)) @call
"#;

static CLIENT_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    compile_client_query(&lang, CLIENT_QUERY_SRC, "grpc_rust")
});

static QUERY: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    Query::new(&lang, QUERY_SRC).expect("grpc_rust: compile QUERY_SRC")
});

pub fn extract_grpc(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    if let Err(e) = parser.set_language(&lang) {
        tracing::warn!("group::extract_grpc (rust): set_language failed: {e:?}");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!(
            "group::extract_grpc (rust): parser.parse returned None for {}",
            file_path.display()
        );
        return Vec::new();
    };
    let query: &tree_sitter::Query = &QUERY;

    let fn_idx = match query.capture_index_for_name("add_fn") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let svc_idx = match query.capture_index_for_name("svc_server") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let new_idx = match query.capture_index_for_name("new_fn") {
        Some(i) => i,
        None => return Vec::new(),
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    let mut out: Vec<ExtractedContract> = Vec::new();

    while let Some(m) = matches.next() {
        let fn_name = super::capture_text(m, fn_idx, source);
        if fn_name != "add_service" {
            continue;
        }
        let new_fn = super::capture_text(m, new_idx, source);
        if new_fn != "new" {
            continue;
        }
        let svc_server = super::capture_text(m, svc_idx, source);
        let Some(svc) = svc_server.strip_suffix("Server") else {
            continue;
        };
        if svc.is_empty() {
            continue;
        }
        out.push(ExtractedContract {
            contract_id: format!("grpc:{svc}:*"),
            contract_type: ContractType::Grpc,
            role: ContractRole::Provider,
            symbol_uid: format!("{}::add_service::{svc}Server", file_path.display()),
            symbol_ref: SymbolRef {
                file_path: file_path.display().to_string(),
                name: "add_service".into(),
            },
            confidence: SERVICE_CONFIDENCE,
            service: None,
            meta: vec![("service".into(), svc.to_string())],
        });
    }
    extract_clients(file_path, source, &tree, &mut out);
    out
}

/// `Consumer` contracts for tonic `<Svc>Client::new` / `connect`. A file that
/// never mentions `tonic` is skipped, because `HttpClient::new()` has the same
/// shape.
fn extract_clients(
    file_path: &Path,
    source: &[u8],
    tree: &tree_sitter::Tree,
    out: &mut Vec<ExtractedContract>,
) {
    if !source.windows(5).any(|w| w == b"tonic") {
        return;
    }
    let Some(query) = CLIENT_QUERY.as_ref() else {
        return;
    };
    let idx = |name: &str| query.capture_index_for_name(name);
    let (Some(client_idx), Some(ctor_idx), Some(call_idx)) =
        (idx("svc_client"), idx("ctor"), idx("call"))
    else {
        return;
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        let ctor = super::capture_text(m, ctor_idx, source);
        if !matches!(ctor, "new" | "connect" | "with_interceptor") {
            continue;
        }
        let client = super::capture_text(m, client_idx, source);
        let Some(svc) = client.strip_suffix("Client") else {
            continue;
        };
        if svc.is_empty() {
            continue;
        }
        out.push(consumer_contract(
            file_path,
            ContractType::Grpc,
            format!("grpc:{svc}:*"),
            &format!("{client}::{ctor}"),
            capture_line(m, call_idx),
            vec![("service".into(), svc.to_string())],
        ));
    }
}
