//! Node/TypeScript HTTP route extractor: Express + Fastify patterns via tree-sitter.

use super::{capture_line, client_verb, compile_client_query, consumer_contract, http_url_path};
use crate::commands::group::types::{ContractRole, ContractType, ExtractedContract, SymbolRef};
use std::path::Path;
use std::sync::LazyLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub(super) const ROUTE_CONFIDENCE: f32 = 0.85;

/// Matches `app.get("/path", handler)` / `router.post(...)` etc.
/// `string_fragment` is already unquoted in tree-sitter-typescript.
const QUERY_SRC: &str = r#"
(call_expression
  function: (member_expression
    object: (_) @recv
    property: (property_identifier) @method)
  arguments: (arguments
    (string
      (string_fragment) @path)
    (_) @handler))
"#;

/// Client calls: `axios.get("/x")` and a bare `fetch("/x")`. Template
/// strings (`fetch(`/x/${id}`)`) and non-literal URLs are not matched.
const CLIENT_QUERY_SRC: &str = r#"
(call_expression
  function: (member_expression
    object: (_) @recv
    property: (property_identifier) @method)
  arguments: (arguments . (string (string_fragment) @path))) @call

(call_expression
  function: (identifier) @callee
  arguments: (arguments . (string (string_fragment) @path))) @call
"#;

/// Receivers that name an HTTP client. `app.get("/x", handler)` and
/// `axios.get("/x", config)` have the same shape; the receiver tells them apart.
const CLIENT_RECEIVERS: &[&str] = &[
    "axios",
    "got",
    "ky",
    "superagent",
    "request",
    "$http",
    "httpClient",
];

static QUERY: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    Query::new(&lang, QUERY_SRC).expect("http_node: compile QUERY_SRC")
});

static CLIENT_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    compile_client_query(&lang, CLIENT_QUERY_SRC, "http_node")
});

pub fn extract_http(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    if let Err(e) = parser.set_language(&lang) {
        tracing::warn!("group::extract_http (node): set_language failed: {e:?}");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!(
            "group::extract_http (node): parser.parse returned None for {}",
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
        let method_name = super::capture_text(m, method_idx, source);
        let Some(http_method) = http_method_from_property(method_name) else {
            continue;
        };
        if recv_idx.is_some_and(|i| CLIENT_RECEIVERS.contains(&super::capture_text(m, i, source))) {
            continue;
        }
        let route_path = super::capture_text(m, path_idx, source);
        // handler capture is the third arg — could be arrow function, identifier, etc.
        // Use its text as a symbol hint; for arrow functions this is verbose but honest.
        let handler_text = super::capture_text(m, handler_idx, source);
        let handler_name = first_identifier(handler_text);
        let id = format!("http:{http_method}:{route_path}");
        out.push(ExtractedContract {
            contract_id: id,
            contract_type: ContractType::Http,
            role: ContractRole::Provider,
            symbol_uid: format!("{}::{handler_name}", file_path.display()),
            symbol_ref: SymbolRef {
                file_path: file_path.display().to_string(),
                name: handler_name.to_string(),
            },
            confidence: ROUTE_CONFIDENCE,
            service: None,
            meta: vec![("method".into(), http_method.into())],
        });
    }
    extract_clients(file_path, source, &tree, &mut out);
    out
}

/// `Consumer` contracts for `axios.get("/x")`, `fetch("/x")` and the like.
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
    let (Some(recv_idx), Some(method_idx), Some(callee_idx), Some(path_idx), Some(call_idx)) = (
        idx("recv"),
        idx("method"),
        idx("callee"),
        idx("path"),
        idx("call"),
    ) else {
        return;
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        let recv = super::capture_text(m, recv_idx, source);
        let (verb, callee) = if recv.is_empty() {
            let callee = super::capture_text(m, callee_idx, source);
            if callee != "fetch" {
                continue;
            }
            (
                fetch_options_method(super::capture_text(m, call_idx, source)),
                callee.to_string(),
            )
        } else {
            let property = super::capture_text(m, method_idx, source);
            let Some(verb) = client_verb(property) else {
                continue;
            };
            if !CLIENT_RECEIVERS.contains(&recv) {
                continue;
            }
            (verb, format!("{recv}.{property}"))
        };
        let Some(path) = http_url_path(super::capture_text(m, path_idx, source)) else {
            continue;
        };
        out.push(consumer_contract(
            file_path,
            ContractType::Http,
            format!("http:{verb}:{path}"),
            &callee,
            capture_line(m, call_idx),
            vec![("method".into(), verb.into())],
        ));
    }
}

/// The verb in `fetch(url, { method: "POST" })`; `GET` when the options do
/// not name one literally.
fn fetch_options_method(call_text: &str) -> &'static str {
    let Some(at) = call_text.find("method") else {
        return "GET";
    };
    let Some(rest) = call_text[at + "method".len()..]
        .trim_start()
        .strip_prefix(':')
    else {
        return "GET";
    };
    let word: String = rest
        .trim_start()
        .trim_start_matches(['"', '\'', '`'])
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    client_verb(&word).unwrap_or("GET")
}

/// Maps Express/Fastify method property name → HTTP method string.
/// Returns `None` for non-route properties (e.g. `listen`, `use`, `json`, ...).
fn http_method_from_property(name: &str) -> Option<&'static str> {
    match name {
        "get" => Some("GET"),
        "post" => Some("POST"),
        "put" => Some("PUT"),
        "delete" => Some("DELETE"),
        "patch" => Some("PATCH"),
        "all" => Some("ANY"),
        _ => None,
    }
}

/// Best-effort: extract leading identifier from handler text (e.g. `myHandler` from
/// `myHandler` or `<unknown>` from an arrow function literal).
fn first_identifier(text: &str) -> &str {
    let trimmed = text.trim();
    // If it starts with a letter/underscore, take the identifier prefix.
    let end = trimmed
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(trimmed.len());
    if end > 0 {
        &trimmed[..end]
    } else {
        "<unknown>"
    }
}
