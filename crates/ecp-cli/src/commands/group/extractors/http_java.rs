//! Java HTTP route extractor: Spring MVC annotation patterns via tree-sitter.

use super::{capture_line, client_verb, compile_client_query, consumer_contract, http_url_path};
use crate::commands::group::types::{ContractRole, ContractType, ExtractedContract, SymbolRef};
use std::path::Path;
use std::sync::LazyLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub(super) const ROUTE_CONFIDENCE: f32 = 0.85;

/// Matches `@PostMapping("/path")` etc. inside a method_declaration.
/// `string_fragment` inside `string_literal` is already unquoted.
/// The method_declaration's name captures the handler symbol.
const QUERY_SRC: &str = r#"
(method_declaration
  (modifiers
    (annotation
      name: (identifier) @ann
      arguments: (annotation_argument_list
        (string_literal
          (string_fragment) @path))))
  name: (identifier) @handler)
"#;

/// Client calls with a literal URL: RestTemplate `getForObject("/x", ...)` and
/// WebClient `webClient.get().uri("/x")`. The second pattern also captures the
/// verb call that precedes `uri`.
const CLIENT_QUERY_SRC: &str = r#"
(method_invocation
  object: (_)
  name: (identifier) @callee
  arguments: (argument_list . (string_literal (string_fragment) @path))) @call

(method_invocation
  object: (method_invocation name: (identifier) @verb)
  name: (identifier) @callee
  arguments: (argument_list . (string_literal (string_fragment) @path))) @call
"#;

static CLIENT_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    compile_client_query(&lang, CLIENT_QUERY_SRC, "http_java")
});

static QUERY: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    Query::new(&lang, QUERY_SRC).expect("http_java: compile QUERY_SRC")
});

pub fn extract_http(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    if let Err(e) = parser.set_language(&lang) {
        tracing::warn!("group::extract_http (java): set_language failed: {e:?}");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!(
            "group::extract_http (java): parser.parse returned None for {}",
            file_path.display()
        );
        return Vec::new();
    };
    let query: &tree_sitter::Query = &QUERY;

    let ann_idx = match query.capture_index_for_name("ann") {
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
        let ann_name = super::capture_text(m, ann_idx, source);
        let Some(http_method) = http_method_from_annotation(ann_name) else {
            continue;
        };
        let route_path = super::capture_text(m, path_idx, source);
        let handler = super::capture_text(m, handler_idx, source);
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

/// `Consumer` contracts for RestTemplate and WebClient calls with a literal URL.
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
    let (Some(callee_idx), Some(verb_idx), Some(path_idx), Some(call_idx)) =
        (idx("callee"), idx("verb"), idx("path"), idx("call"))
    else {
        return;
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        let callee = super::capture_text(m, callee_idx, source);
        let verb = match callee {
            "uri" => client_verb(super::capture_text(m, verb_idx, source)),
            "getForObject" | "getForEntity" => Some("GET"),
            "postForObject" | "postForEntity" | "postForLocation" => Some("POST"),
            "patchForObject" => Some("PATCH"),
            _ => None,
        };
        let Some(verb) = verb else {
            continue;
        };
        let Some(path) = http_url_path(super::capture_text(m, path_idx, source)) else {
            continue;
        };
        out.push(consumer_contract(
            file_path,
            ContractType::Http,
            format!("http:{verb}:{path}"),
            callee,
            capture_line(m, call_idx),
            vec![("method".into(), verb.into())],
        ));
    }
}

/// Maps Spring annotation name → HTTP method string.
/// Returns `None` for non-route annotations (e.g. `PathVariable`, `RestController`).
fn http_method_from_annotation(ann: &str) -> Option<&'static str> {
    match ann {
        "GetMapping" => Some("GET"),
        "PostMapping" => Some("POST"),
        "PutMapping" => Some("PUT"),
        "DeleteMapping" => Some("DELETE"),
        "PatchMapping" => Some("PATCH"),
        "RequestMapping" => Some("ANY"),
        _ => None,
    }
}
