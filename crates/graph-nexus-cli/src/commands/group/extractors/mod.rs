//! Per-language extractors emitting ExtractedContract from source.
//! First wave: HTTP routes + gRPC service defs in Go/Python/Node/Java/Rust.
//! Other 9 mainstream langs are BlindSpot stubs (registered but emit nothing).

pub mod grpc_go;
pub mod grpc_python;
pub mod grpc_node;
pub mod grpc_java;
pub mod grpc_rust;
pub mod http_go;
pub mod http_python;
pub mod http_node;
pub mod http_java;
pub mod http_rust;

use crate::commands::group::types::ExtractedContract;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractorKind { Http, Grpc }

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
        ExtractorEntry { lang: "go",     kind: ExtractorKind::Http, extract: http_go::extract_http },
        ExtractorEntry { lang: "python", kind: ExtractorKind::Http, extract: http_python::extract_http },
        ExtractorEntry { lang: "node",   kind: ExtractorKind::Http, extract: http_node::extract_http },
        ExtractorEntry { lang: "java",   kind: ExtractorKind::Http, extract: http_java::extract_http },
        ExtractorEntry { lang: "rust",   kind: ExtractorKind::Http, extract: http_rust::extract_http },
    ]
}

fn grpc_extractors() -> Vec<ExtractorEntry> {
    vec![
        ExtractorEntry { lang: "go",     kind: ExtractorKind::Grpc, extract: grpc_go::extract_grpc },
        ExtractorEntry { lang: "python", kind: ExtractorKind::Grpc, extract: grpc_python::extract_grpc },
        ExtractorEntry { lang: "node",   kind: ExtractorKind::Grpc, extract: grpc_node::extract_grpc },
        ExtractorEntry { lang: "java",   kind: ExtractorKind::Grpc, extract: grpc_java::extract_grpc },
        ExtractorEntry { lang: "rust",   kind: ExtractorKind::Grpc, extract: grpc_rust::extract_grpc },
    ]
}

/// Extract the UTF-8 text of the capture at `idx` from a query match.
/// Returns `""` when the capture index is absent or the bytes are not valid UTF-8.
pub(super) fn capture_text<'a>(
    m: &tree_sitter::QueryMatch<'a, 'a>,
    idx: u32,
    source: &'a [u8],
) -> &'a str {
    for c in m.captures {
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
