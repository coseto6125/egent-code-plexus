//! Per-language extractors emitting ExtractedContract from source.
//! First wave: HTTP routes + gRPC service defs in Go/Python/Node/Java/Rust.
//! Other 9 mainstream langs are BlindSpot stubs (registered but emit nothing).

pub mod grpc_go;
pub mod grpc_java;
pub mod grpc_node;
pub mod grpc_python;
pub mod grpc_rust;
pub mod http_go;
pub mod http_java;
pub mod http_node;
pub mod http_python;
pub mod http_rust;

use crate::commands::group::types::{ContractRole, ContractType, ExtractedContract, SymbolRef};
use ecp_core::contracts::routing::{Dialect, FileRouter, Language as RouteLanguage};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractorKind {
    Http,
    Grpc,
}

pub struct ExtractorEntry {
    pub lang: &'static str,
    /// Read only by integration tests (group_extractor_registry.rs); release build sees it as dead.
    #[allow(dead_code)]
    pub kind: ExtractorKind,
    pub extract: fn(&Path, &[u8]) -> Vec<ExtractedContract>,
}

pub fn registry() -> Vec<ExtractorEntry> {
    let mut v: Vec<ExtractorEntry> = Vec::new();
    v.extend(http_extractors());
    v.extend(grpc_extractors());
    v
}

fn http_extractors() -> Vec<ExtractorEntry> {
    vec![
        ExtractorEntry {
            lang: "go",
            kind: ExtractorKind::Http,
            extract: http_go::extract_http,
        },
        ExtractorEntry {
            lang: "python",
            kind: ExtractorKind::Http,
            extract: http_python::extract_http,
        },
        ExtractorEntry {
            lang: "node",
            kind: ExtractorKind::Http,
            extract: http_node::extract_http,
        },
        ExtractorEntry {
            lang: "java",
            kind: ExtractorKind::Http,
            extract: http_java::extract_http,
        },
        ExtractorEntry {
            lang: "rust",
            kind: ExtractorKind::Http,
            extract: http_rust::extract_http,
        },
    ]
}

fn grpc_extractors() -> Vec<ExtractorEntry> {
    vec![
        ExtractorEntry {
            lang: "go",
            kind: ExtractorKind::Grpc,
            extract: grpc_go::extract_grpc,
        },
        ExtractorEntry {
            lang: "python",
            kind: ExtractorKind::Grpc,
            extract: grpc_python::extract_grpc,
        },
        ExtractorEntry {
            lang: "node",
            kind: ExtractorKind::Grpc,
            extract: grpc_node::extract_grpc,
        },
        ExtractorEntry {
            lang: "java",
            kind: ExtractorKind::Grpc,
            extract: grpc_java::extract_grpc,
        },
        ExtractorEntry {
            lang: "rust",
            kind: ExtractorKind::Grpc,
            extract: grpc_rust::extract_grpc,
        },
    ]
}

/// Extract the UTF-8 text of the capture at `idx` from a query match.
/// Returns `""` when the capture index is absent or the bytes are not valid UTF-8.
pub(super) fn capture_text<'a>(
    m: &tree_sitter::QueryMatch<'a, 'a>,
    idx: u32,
    source: &'a [u8],
) -> &'a str {
    for c in m.captures() {
        if c.index == idx {
            return std::str::from_utf8(&source[c.node.byte_range()]).unwrap_or("");
        }
    }
    ""
}

/// `(ext, lang)` mapping used by `sync.rs` when walking source files.
/// Centralised here so add-a-language touches one place.
pub fn lang_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "go" => Some("go"),
        "py" => Some("python"),
        "ts" | "tsx" | "js" | "jsx" => Some("node"),
        "java" => Some("java"),
        "rs" => Some("rust"),
        _ => None,
    }
}

/// The grammars the Node extractors parse with.
#[derive(Clone, Copy)]
pub(super) enum NodeGrammar {
    TypeScript,
    Tsx,
}

impl NodeGrammar {
    pub(super) fn language(self) -> tree_sitter::Language {
        match self {
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }
}

/// The grammar for `file_path`, chosen from its C5 route as `flow` and the
/// pattern finder choose theirs. Only a standard-dialect TypeScript route keeps
/// the TypeScript grammar, which reads JSX as type assertions and loses the
/// calls inside JSX attributes. Every other path goes through TSX, a superset
/// that also parses JSX: this crate links no JavaScript grammar, so a
/// JavaScript route cannot take its own yet. An unrouted or excluded path was
/// extracted before this choice existed, so it keeps the TSX default.
pub(super) fn node_grammar(file_path: &Path) -> NodeGrammar {
    let route = file_path
        .to_str()
        .map(|path| FileRouter::PATH_ONLY.route(path));
    let standard_typescript = route
        .as_ref()
        .and_then(|outcome| outcome.route())
        .is_some_and(|route| {
            route.fixed_language() == Some(RouteLanguage::TypeScript)
                && route.dialect() == Dialect::Standard
        });
    if standard_typescript {
        NodeGrammar::TypeScript
    } else {
        NodeGrammar::Tsx
    }
}

/// Confidence of a client-call match. Lower than a route declaration: the
/// extractor sees a string literal at a call site, not the declared endpoint.
pub(super) const CLIENT_CONFIDENCE: f32 = 0.7;

/// The `Consumer` side of a contract. `callee` names the call (`fetch`,
/// `axios.get`); `line` keeps two calls of one callee in one file apart, since
/// `matching.rs` tracks matched contracts by `symbol_uid`.
pub(super) fn consumer_contract(
    file_path: &Path,
    contract_type: ContractType,
    contract_id: String,
    callee: &str,
    line: usize,
    mut meta: Vec<(String, String)>,
) -> ExtractedContract {
    meta.push(("line".into(), line.to_string()));
    ExtractedContract {
        contract_id,
        contract_type,
        role: ContractRole::Consumer,
        symbol_uid: format!("{}::{callee}@{line}", file_path.display()),
        symbol_ref: SymbolRef {
            file_path: file_path.display().to_string(),
            name: callee.to_string(),
        },
        confidence: CLIENT_CONFIDENCE,
        service: None,
        meta,
    }
}

/// Provider and consumer ids meet on `http:<VERB>:<path>`, so a client URL is
/// reduced to its path: the origin and the query string are dropped. `None`
/// for a URL with no path (`https://host`) or a relative path without a
/// leading `/`, which no route declaration can match.
pub(super) fn http_url_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let without_origin = match raw.split_once("://") {
        Some((_, rest)) => &rest[rest.find('/')?..],
        None => raw,
    };
    let path = without_origin
        .split(['?', '#'])
        .next()
        .unwrap_or(without_origin);
    path.starts_with('/').then(|| path.to_string())
}

/// `get` / `GET` / `Get` -> `GET`. `None` for any other name.
pub(super) fn client_verb(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "get" => Some("GET"),
        "post" => Some("POST"),
        "put" => Some("PUT"),
        "delete" => Some("DELETE"),
        "patch" => Some("PATCH"),
        _ => None,
    }
}

/// Compile a client-call query; a query that fails to compile disables the
/// consumer side of one extractor and leaves its providers intact.
pub(super) fn compile_client_query(
    lang: &tree_sitter::Language,
    src: &str,
    who: &str,
) -> Option<tree_sitter::Query> {
    tree_sitter::Query::new(lang, src)
        .map_err(|e| tracing::warn!("group::{who}: client query did not compile: {e}"))
        .ok()
}

/// 1-based source line of the capture at `idx`; `0` when the capture is absent.
pub(super) fn capture_line(m: &tree_sitter::QueryMatch<'_, '_>, idx: u32) -> usize {
    for c in m.captures() {
        if c.index == idx {
            return c.node.start_position().row + 1;
        }
    }
    0
}
