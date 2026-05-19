//! Java gRPC server-registration extractor: new <Svc>ImplBase() inside addService().

use crate::commands::group::types::{
    ContractRole, ContractType, ExtractedContract, SymbolRef,
};
use std::path::Path;
use std::sync::LazyLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub(super) const SERVICE_CONFIDENCE: f32 = 0.9;

/// Matches `addService(new <Outer>.<Inner>() { ... })`.
/// Captures the inner type identifier of the scoped_type_identifier — that's
/// `<Svc>ImplBase`; we strip the `ImplBase` suffix in Rust.
/// Using two captures on scoped_type_identifier children and keeping the last.
const QUERY_SRC: &str = r#"
(method_invocation
  name: (identifier) @add_fn
  arguments: (argument_list
    (object_creation_expression
      type: (scoped_type_identifier
        (type_identifier) @outer_class
        (type_identifier) @impl_class))))
"#;

static QUERY: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    Query::new(&lang, QUERY_SRC).expect("grpc_java: compile QUERY_SRC")
});

pub fn extract_grpc(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    if let Err(e) = parser.set_language(&lang) {
        tracing::warn!("group::extract_grpc (java): set_language failed: {e:?}");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!(
            "group::extract_grpc (java): parser.parse returned None for {}",
            file_path.display()
        );
        return Vec::new();
    };
    let query: &tree_sitter::Query = &QUERY;

    let fn_idx = match query.capture_index_for_name("add_fn") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let impl_idx = match query.capture_index_for_name("impl_class") {
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
        let impl_class = super::capture_text(m, impl_idx, source);
        let Some(svc) = impl_class.strip_suffix("ImplBase") else {
            continue;
        };
        if svc.is_empty() {
            continue;
        }
        out.push(ExtractedContract {
            contract_id: format!("grpc:{svc}:*"),
            contract_type: ContractType::Grpc,
            role: ContractRole::Provider,
            symbol_uid: format!("{}::addService::{svc}ImplBase", file_path.display()),
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

