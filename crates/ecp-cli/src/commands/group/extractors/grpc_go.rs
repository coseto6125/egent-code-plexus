//! Go gRPC server-registration extractor: captures Register<Svc>Server calls via tree-sitter.

use crate::commands::group::types::{ContractRole, ContractType, ExtractedContract, SymbolRef};
use std::path::Path;
use std::sync::LazyLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// First-wave confidence for gRPC server registration — higher than HTTP 0.85
/// because `Register<Svc>Server` is unambiguous generated code.
pub(super) const SERVICE_CONFIDENCE: f32 = 0.9;

/// Matches any `<expr>.Register<Svc>Server(...)` call expression.
/// We capture the selector field name and extract `<Svc>` by stripping
/// the "Register" prefix and "Server" suffix.
const QUERY_SRC: &str = r#"
(call_expression
  function: (selector_expression
              field: (field_identifier) @register_fn))
"#;

static QUERY: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    Query::new(&lang, QUERY_SRC).expect("grpc_go: compile QUERY_SRC")
});

pub fn extract_grpc(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    if let Err(e) = parser.set_language(&lang) {
        tracing::warn!("group::extract_grpc (go): set_language failed: {e:?}");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!(
            "group::extract_grpc (go): parser.parse returned None for {}",
            file_path.display()
        );
        return Vec::new();
    };
    let query: &tree_sitter::Query = &QUERY;

    let fn_idx = match query.capture_index_for_name("register_fn") {
        Some(i) => i,
        None => return Vec::new(),
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    let mut out: Vec<ExtractedContract> = Vec::new();

    while let Some(m) = matches.next() {
        let fn_name = super::capture_text(m, fn_idx, source);
        let Some(svc) = parse_register_fn(fn_name) else {
            continue;
        };
        out.push(ExtractedContract {
            contract_id: format!("grpc:{svc}:*"),
            contract_type: ContractType::Grpc,
            role: ContractRole::Provider,
            symbol_uid: format!("{}::Register{svc}Server", file_path.display()),
            symbol_ref: SymbolRef {
                file_path: file_path.display().to_string(),
                name: format!("Register{svc}Server"),
            },
            confidence: SERVICE_CONFIDENCE,
            service: None,
            meta: vec![("service".into(), svc.into())],
        });
    }
    out
}

/// Returns `Some("UserService")` for `"RegisterUserServiceServer"`, `None` otherwise.
fn parse_register_fn(name: &str) -> Option<&str> {
    let after_register = name.strip_prefix("Register")?;
    let svc = after_register.strip_suffix("Server")?;
    if svc.is_empty() {
        return None;
    }
    Some(svc)
}
