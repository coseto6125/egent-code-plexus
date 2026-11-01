//! Node/TypeScript gRPC server-registration extractor: server.addService(<svc>_proto.<Svc>.service).

use super::{node_grammar, NodeGrammar};
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

/// One grammar with the query compiled for it: a query is bound to the
/// language it was compiled with, so each dialect owns its own.
struct Dialect {
    language: tree_sitter::Language,
    query: Query,
}

impl Dialect {
    fn new(language: tree_sitter::Language) -> Self {
        Self {
            query: Query::new(&language, QUERY_SRC).expect("grpc_node: compile QUERY_SRC"),
            language,
        }
    }
}

static TYPESCRIPT: LazyLock<Dialect> =
    LazyLock::new(|| Dialect::new(NodeGrammar::TypeScript.language()));
static TSX: LazyLock<Dialect> = LazyLock::new(|| Dialect::new(NodeGrammar::Tsx.language()));

fn dialect_for(file_path: &Path) -> &'static Dialect {
    match node_grammar(file_path) {
        NodeGrammar::TypeScript => &TYPESCRIPT,
        NodeGrammar::Tsx => &TSX,
    }
}

pub fn extract_grpc(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let dialect = dialect_for(file_path);
    let query = &dialect.query;
    let mut parser = Parser::new();
    if let Err(e) = parser.set_language(&dialect.language) {
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
