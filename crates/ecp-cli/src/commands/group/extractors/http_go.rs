//! Go HTTP route extractor: net/http + gin + chi shapes via tree-sitter.

use super::{capture_line, client_verb, compile_client_query, consumer_contract, http_url_path};
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
              operand: (_) @recv
              field: (field_identifier) @method)
  arguments: (argument_list
               (interpreted_string_literal) @path
               .
               (_) @handler))
"#;

/// Client calls with a literal URL: `http.Get("/x")`, `client.Post("/x", ...)`.
const CLIENT_QUERY_SRC: &str = r#"
(call_expression
  function: (selector_expression
              operand: (_) @recv
              field: (field_identifier) @method)
  arguments: (argument_list . (interpreted_string_literal) @path)) @call
"#;

static CLIENT_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    compile_client_query(&lang, CLIENT_QUERY_SRC, "http_go")
});

/// A receiver is an HTTP client when it is the `http` package or its name
/// ends in `client`. `r.Get("/x", h)` (chi) has the same shape; the receiver
/// tells it apart.
fn is_client_receiver(recv: &str) -> bool {
    recv == "http" || recv.to_ascii_lowercase().ends_with("client")
}

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

    let recv_idx = query.capture_index_for_name("recv");

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    let mut out: Vec<ExtractedContract> = Vec::new();

    while let Some(m) = matches.next() {
        let method = super::capture_text(m, method_idx, source);
        if !is_route_register(method) {
            continue;
        }
        if recv_idx.is_some_and(|i| is_client_receiver(super::capture_text(m, i, source))) {
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
    extract_clients(file_path, source, &tree, &mut out);
    out
}

/// `Consumer` contracts for `http.Get("/x")` and `<name>Client.Post("/x", ...)`.
fn extract_clients(
    file_path: &Path,
    source: &[u8],
    tree: &tree_sitter::Tree,
    out: &mut Vec<ExtractedContract>,
) {
    let Some(query) = CLIENT_QUERY.as_ref() else {
        return;
    };
    let idx = |name: &str| query.capture_index_for_name(name);
    let (Some(recv_idx), Some(method_idx), Some(path_idx), Some(call_idx)) =
        (idx("recv"), idx("method"), idx("path"), idx("call"))
    else {
        return;
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        let recv = super::capture_text(m, recv_idx, source);
        let method = super::capture_text(m, method_idx, source);
        if !is_client_receiver(recv) {
            continue;
        }
        let Some(verb) = client_verb(method) else {
            continue;
        };
        let Some(path) = http_url_path(&unquote(super::capture_text(m, path_idx, source))) else {
            continue;
        };
        out.push(consumer_contract(
            file_path,
            ContractType::Http,
            format!("http:{verb}:{path}"),
            &format!("{recv}.{method}"),
            capture_line(m, call_idx),
            vec![("method".into(), verb.into())],
        ));
    }
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
