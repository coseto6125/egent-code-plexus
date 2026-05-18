//! Go HTTP route extractor: net/http + gin + chi shapes via tree-sitter.

use crate::commands::group::types::{ContractRole, ContractType, ExtractedContract, SymbolRef};
use std::path::Path;
use std::sync::LazyLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// First-wave heuristic confidence for Strategy-B extractor matches. Graph-assisted Strategy A (T?) will bump this to 1.0.
pub(super) const ROUTE_CONFIDENCE: f32 = 0.85;

const QUERY_SRC: &str = r#"
(call_expression
  function: (selector_expression
              field: (field_identifier) @method)
  arguments: (argument_list
               (interpreted_string_literal) @path
               .
               (_) @handler))
"#;

static QUERY: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    Query::new(&lang, QUERY_SRC).expect("http_go: compile QUERY_SRC")
});

pub fn extract_http(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    if let Err(e) = parser.set_language(&lang) {
        tracing::warn!("group::extract_http (go): set_language failed: {e:?}");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!(
            "group::extract_http (go): parser.parse returned None for {}",
            file_path.display()
        );
        return Vec::new();
    };
    let query: &tree_sitter::Query = &QUERY;

    let method_idx = match query.capture_index_for_name("method") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let path_idx = match query.capture_index_for_name("path") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let handler_idx = match query.capture_index_for_name("handler") {
        Some(i) => i,
        None => return Vec::new(),
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    let mut out: Vec<ExtractedContract> = Vec::new();

    while let Some(m) = matches.next() {
        let method = super::capture_text(m, method_idx, source);
        if !is_route_register(method) {
            continue;
        }
        let raw_path = super::capture_text(m, path_idx, source);
        let route_path = unquote(raw_path);
        let handler = super::capture_text(m, handler_idx, source);
        let http_method = http_method_from_call(method);
        let id = format!("http:{http_method}:{route_path}");
        out.push(ExtractedContract {
            contract_id: id,
            contract_type: ContractType::Http,
            role: ContractRole::Provider,
            symbol_uid: format!("{}::{handler}", file_path.display()),
            symbol_ref: SymbolRef {
                file_path: file_path.display().to_string(),
                name: handler.to_string(),
            },
            confidence: ROUTE_CONFIDENCE,
            service: None,
            meta: vec![("method".into(), http_method.into())],
        });
    }
    out
}

fn is_route_register(method: &str) -> bool {
    matches!(
        method,
        "HandleFunc"
            | "Handle"
            | "GET"
            | "POST"
            | "PUT"
            | "DELETE"
            | "PATCH"
            | "Get"
            | "Post"
            | "Put"
            | "Delete"
            | "Patch"
    )
}

fn http_method_from_call(method: &str) -> &'static str {
    match method {
        "GET" | "Get" => "GET",
        "POST" | "Post" => "POST",
        "PUT" | "Put" => "PUT",
        "DELETE" | "Delete" => "DELETE",
        "PATCH" | "Patch" => "PATCH",
        _ => "ANY",
    }
}

fn unquote(s: &str) -> String {
    s.trim_start_matches('"').trim_end_matches('"').to_string()
}
