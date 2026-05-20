//! Node/TypeScript gRPC server-registration extractor: server.addService(<svc>_proto.<Svc>.service).

use crate::commands::group::types::{ContractRole, ContractType, ExtractedContract, SymbolRef};
use std::path::Path;
use std::sync::LazyLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub(super) const SERVICE_CONFIDENCE: f32 = 0.9;

/// Matches `server.addService(<pkg>.<Svc>.service, ...)`.
/// Captures the outer method name (`addService`) and the service class name
/// (`<Svc>` — the intermediate member property before `.service`).
const QUERY_SRC: &str = r#"
(call_expression
  function: (member_expression
    property: (property_identifier) @add_fn)
  arguments: (arguments
    (member_expression
      object: (member_expression
        property: (property_identifier) @svc_name)
      property: (property_identifier) @service_field)))
"#;

static QUERY: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    Query::new(&lang, QUERY_SRC).expect("grpc_node: compile QUERY_SRC")
});

pub fn extract_grpc(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    if let Err(e) = parser.set_language(&lang) {
        tracing::warn!("group::extract_grpc (node): set_language failed: {e:?}");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!(
            "group::extract_grpc (node): parser.parse returned None for {}",
            file_path.display()
        );
        return Vec::new();
    };
    let query: &tree_sitter::Query = &QUERY;

    let fn_idx = match query.capture_index_for_name("add_fn") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let svc_idx = match query.capture_index_for_name("svc_name") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let field_idx = match query.capture_index_for_name("service_field") {
        Some(i) => i,
        None => return Vec::new(),
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    let mut out: Vec<ExtractedContract> = Vec::new();

    while let Some(m) = matches.next() {
        let fn_name = super::capture_text(m, fn_idx, source);
        if fn_name != "addService" {
            continue;
        }
        let service_field = super::capture_text(m, field_idx, source);
        if service_field != "service" {
            continue;
        }
        let svc = super::capture_text(m, svc_idx, source);
        if svc.is_empty() {
            continue;
        }
        out.push(ExtractedContract {
            contract_id: format!("grpc:{svc}:*"),
            contract_type: ContractType::Grpc,
            role: ContractRole::Provider,
            symbol_uid: format!("{}::addService::{svc}", file_path.display()),
            symbol_ref: SymbolRef {
                file_path: file_path.display().to_string(),
                name: "addService".into(),
            },
            confidence: SERVICE_CONFIDENCE,
            service: None,
            meta: vec![("service".into(), svc.to_string())],
        });
    }
    out
}
