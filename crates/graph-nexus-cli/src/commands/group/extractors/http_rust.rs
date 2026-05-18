//! Rust HTTP route extractor: axum `.route()` + actix-web attribute macros via tree-sitter.

use crate::commands::group::types::{ContractRole, ContractType, ExtractedContract, SymbolRef};
use std::path::Path;
use std::sync::LazyLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub(super) const ROUTE_CONFIDENCE: f32 = 0.85;

/// axum: `.route("/path", post(handler))` pattern.
/// Anchors on the call_expression whose function is a field_expression with field "route",
/// then captures the string_content and the inner method-call's function + handler.
const AXUM_QUERY: &str = r#"
(call_expression
  function: (field_expression
    field: (field_identifier) @route_fn)
  arguments: (arguments
    (string_literal
      (string_content) @path)
    (call_expression
      function: (identifier) @method
      arguments: (arguments
        (identifier) @handler))))
"#;

/// actix-web: `#[post("/path")]` attribute on a function item.
/// `string_content` inside `token_tree` > `string_literal` gives the unquoted path.
// Handler name is NOT captured in this query — actix attributes (`#[get("/path")]`)
// are siblings of the function_item they decorate, not parents. We resolve the
// handler via `next_named_sibling()` walk in `actix_handler_name` instead.
const ACTIX_QUERY: &str = r#"
(attribute_item
  (attribute
    (identifier) @method
    arguments: (token_tree
      (string_literal
        (string_content) @path))))
"#;

static AXUM_QUERY_COMPILED: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    Query::new(&lang, AXUM_QUERY).expect("http_rust: compile AXUM_QUERY")
});

static ACTIX_QUERY_COMPILED: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    Query::new(&lang, ACTIX_QUERY).expect("http_rust: compile ACTIX_QUERY")
});

pub fn extract_http(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    if let Err(e) = parser.set_language(&lang) {
        tracing::warn!("group::extract_http (rust): set_language failed: {e:?}");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!(
            "group::extract_http (rust): parser.parse returned None for {}",
            file_path.display()
        );
        return Vec::new();
    };

    let mut out: Vec<ExtractedContract> = Vec::new();
    extract_axum(file_path, source, &tree, &lang, &mut out);
    extract_actix(file_path, source, &tree, &lang, &mut out);
    out
}

fn extract_axum(
    file_path: &Path,
    source: &[u8],
    tree: &tree_sitter::Tree,
    _lang: &tree_sitter::Language,
    out: &mut Vec<ExtractedContract>,
) {
    let query: &tree_sitter::Query = &AXUM_QUERY_COMPILED;

    let route_fn_idx = match query.capture_index_for_name("route_fn") {
        Some(i) => i,
        None => return,
    };
    let path_idx = match query.capture_index_for_name("path") {
        Some(i) => i,
        None => return,
    };
    let method_idx = match query.capture_index_for_name("method") {
        Some(i) => i,
        None => return,
    };
    let handler_idx = match query.capture_index_for_name("handler") {
        Some(i) => i,
        None => return,
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);

    while let Some(m) = matches.next() {
        let route_fn = super::capture_text(m, route_fn_idx, source);
        if route_fn != "route" {
            continue;
        }
        let method_name = super::capture_text(m, method_idx, source);
        let Some(http_method) = http_method_from_fn(method_name) else {
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
}

fn extract_actix(
    file_path: &Path,
    source: &[u8],
    tree: &tree_sitter::Tree,
    _lang: &tree_sitter::Language,
    out: &mut Vec<ExtractedContract>,
) {
    let query: &tree_sitter::Query = &ACTIX_QUERY_COMPILED;

    let method_idx = match query.capture_index_for_name("method") {
        Some(i) => i,
        None => return,
    };
    let path_idx = match query.capture_index_for_name("path") {
        Some(i) => i,
        None => return,
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);

    while let Some(m) = matches.next() {
        let method_name = super::capture_text(m, method_idx, source);
        let Some(http_method) = http_method_from_fn(method_name) else {
            continue;
        };
        let route_path = super::capture_text(m, path_idx, source);
        // For actix, the function name follows the attribute_item as a sibling function_item.
        // Resolve via next-sibling walk on the attribute's parent node.
        let handler = actix_handler_name(m, method_idx, source);
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
}

/// Walk the attribute_item's next named sibling to find a function_item and extract its name.
fn actix_handler_name<'a>(
    m: &tree_sitter::QueryMatch<'a, 'a>,
    method_idx: u32,
    source: &'a [u8],
) -> String {
    // Find the attribute_item node (parent of the attribute holding @method).
    for c in m.captures {
        if c.index == method_idx {
            // c.node is the (identifier) for the method name inside (attribute ...)
            // Walk up: identifier → attribute → attribute_item
            let attr_item = c.node.parent().and_then(|a| a.parent());
            if let Some(item) = attr_item {
                let mut sibling = item.next_named_sibling();
                while let Some(s) = sibling {
                    if s.kind() == "function_item" {
                        // function_item > name: (identifier)
                        for i in 0..s.named_child_count() {
                            if let Some(child) = s.named_child(i) {
                                if child.kind() == "identifier" {
                                    return std::str::from_utf8(&source[child.byte_range()])
                                        .unwrap_or("<unknown>")
                                        .to_string();
                                }
                            }
                        }
                    }
                    sibling = s.next_named_sibling();
                }
            }
            break;
        }
    }
    "<unknown>".to_string()
}

/// Maps axum/actix function name → HTTP method string.
/// Returns `None` for non-route functions.
fn http_method_from_fn(name: &str) -> Option<&'static str> {
    match name {
        "get" => Some("GET"),
        "post" => Some("POST"),
        "put" => Some("PUT"),
        "delete" => Some("DELETE"),
        "patch" => Some("PATCH"),
        "any" => Some("ANY"),
        _ => None,
    }
}
