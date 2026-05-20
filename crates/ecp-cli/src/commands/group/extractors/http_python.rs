//! Python HTTP route extractor: Flask + FastAPI decorator patterns via tree-sitter.

use crate::commands::group::types::{ContractRole, ContractType, ExtractedContract, SymbolRef};
use std::path::Path;
use std::sync::LazyLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub(super) const ROUTE_CONFIDENCE: f32 = 0.85;

/// Matches `@app.get("/path")` / `@router.post("/path")` — verb from decorator name.
/// `string_content` is already unquoted in tree-sitter-python.
const QUERY_VERB: &str = r#"
(decorated_definition
  (decorator
    (call
      function: (attribute
        attribute: (identifier) @method)
      arguments: (argument_list
        (string
          (string_content) @path))))
  definition: (function_definition
    name: (identifier) @handler))
"#;

/// Matches `@app.route("/path", methods=["POST", ...])` — verb from methods list.
/// Also matches `@app.route("/path")` without methods (handler still captured).
/// A second query for the `methods=` keyword_argument extracts the first method string.
const QUERY_ROUTE_PATH: &str = r#"
(decorated_definition
  (decorator
    (call
      function: (attribute
        attribute: (identifier) @route_kw)
      arguments: (argument_list
        (string
          (string_content) @path))))
  definition: (function_definition
    name: (identifier) @handler))
"#;

/// Companion: find `methods=["POST"]` inside `@app.route(...)` call arguments.
const QUERY_ROUTE_METHODS: &str = r#"
(decorated_definition
  (decorator
    (call
      arguments: (argument_list
        (keyword_argument
          name: (identifier) @kw_name
          value: (list
            (string
              (string_content) @http_method))))))
  definition: (function_definition
    name: (identifier) @handler))
"#;

static QUERY_VERB_COMPILED: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    Query::new(&lang, QUERY_VERB).expect("http_python: compile QUERY_VERB")
});

static QUERY_ROUTE_PATH_COMPILED: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    Query::new(&lang, QUERY_ROUTE_PATH).expect("http_python: compile QUERY_ROUTE_PATH")
});

static QUERY_ROUTE_METHODS_COMPILED: LazyLock<Query> = LazyLock::new(|| {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    Query::new(&lang, QUERY_ROUTE_METHODS).expect("http_python: compile QUERY_ROUTE_METHODS")
});

pub fn extract_http(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    if let Err(e) = parser.set_language(&lang) {
        tracing::warn!("group::extract_http (python): set_language failed: {e:?}");
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!(
            "group::extract_http (python): parser.parse returned None for {}",
            file_path.display()
        );
        return Vec::new();
    };

    // Pass 1: FastAPI / verb-in-decorator-name routes (@app.get, @router.post, ...).
    let mut out = extract_verb_routes(file_path, source, &tree, &lang);

    // Pass 2: Flask @app.route(...) — collect handler → path first, then overlay methods.
    extract_flask_routes(file_path, source, &tree, &lang, &mut out);

    out
}

fn extract_verb_routes(
    file_path: &Path,
    source: &[u8],
    tree: &tree_sitter::Tree,
    _lang: &tree_sitter::Language,
) -> Vec<ExtractedContract> {
    let query: &tree_sitter::Query = &QUERY_VERB_COMPILED;

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
        let method_name = super::capture_text(m, method_idx, source);
        // Only emit for non-route verb decorators; `route` is handled by pass 2.
        let Some(http_method) = http_verb_from_decorator(method_name) else {
            continue;
        };
        let route_path = super::capture_text(m, path_idx, source);
        let handler = super::capture_text(m, handler_idx, source);
        push_contract(file_path, route_path, http_method, handler, &mut out);
    }
    out
}

fn extract_flask_routes(
    file_path: &Path,
    source: &[u8],
    tree: &tree_sitter::Tree,
    _lang: &tree_sitter::Language,
    out: &mut Vec<ExtractedContract>,
) {
    // Step 1: collect all @app.route(...) handler → path pairs.
    let path_query: &tree_sitter::Query = &QUERY_ROUTE_PATH_COMPILED;

    let route_kw_idx = match path_query.capture_index_for_name("route_kw") {
        Some(i) => i,
        None => return,
    };
    let path_idx = match path_query.capture_index_for_name("path") {
        Some(i) => i,
        None => return,
    };
    let handler_idx = match path_query.capture_index_for_name("handler") {
        Some(i) => i,
        None => return,
    };

    // handler_name → route_path (first occurrence wins).
    let mut handler_to_path: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(path_query, tree.root_node(), source);
        while let Some(m) = matches.next() {
            let kw = super::capture_text(m, route_kw_idx, source);
            if kw != "route" {
                continue;
            }
            let path = super::capture_text(m, path_idx, source).to_string();
            let handler = super::capture_text(m, handler_idx, source).to_string();
            handler_to_path.entry(handler).or_insert(path);
        }
    }

    if handler_to_path.is_empty() {
        return;
    }

    // Step 2: collect methods=[...] per handler.
    let methods_query: &tree_sitter::Query = &QUERY_ROUTE_METHODS_COMPILED;

    let kw_name_idx = match methods_query.capture_index_for_name("kw_name") {
        Some(i) => i,
        None => return,
    };
    let http_method_idx = match methods_query.capture_index_for_name("http_method") {
        Some(i) => i,
        None => return,
    };
    let m_handler_idx = match methods_query.capture_index_for_name("handler") {
        Some(i) => i,
        None => return,
    };

    let mut handler_to_methods: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(methods_query, tree.root_node(), source);
        while let Some(m) = matches.next() {
            let kw_name = super::capture_text(m, kw_name_idx, source);
            if kw_name != "methods" {
                continue;
            }
            let http_method = super::capture_text(m, http_method_idx, source).to_uppercase();
            let handler = super::capture_text(m, m_handler_idx, source).to_string();
            handler_to_methods
                .entry(handler)
                .or_default()
                .push(http_method);
        }
    }

    // Step 3: emit one contract per handler, using methods if found or ANY.
    for (handler, path) in &handler_to_path {
        if let Some(methods) = handler_to_methods.get(handler) {
            for method in methods {
                push_contract(file_path, path, method.as_str(), handler.as_str(), out);
            }
        } else {
            push_contract(file_path, path, "ANY", handler.as_str(), out);
        }
    }
}

fn push_contract(
    file_path: &Path,
    route_path: &str,
    http_method: &str,
    handler: &str,
    out: &mut Vec<ExtractedContract>,
) {
    out.push(ExtractedContract {
        contract_id: format!("http:{http_method}:{route_path}"),
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

/// Maps FastAPI/Blueprint verb decorator name → HTTP method.
/// Excludes `route` — handled by Flask pass.
fn http_verb_from_decorator(name: &str) -> Option<&'static str> {
    match name {
        "get" => Some("GET"),
        "post" => Some("POST"),
        "put" => Some("PUT"),
        "delete" => Some("DELETE"),
        "patch" => Some("PATCH"),
        _ => None,
    }
}
