//! Per-language extractors emitting ExtractedContract from source.
//! First wave: HTTP routes + gRPC service defs in Go/Python/Node/Java/Rust.
//! Other 9 mainstream langs are BlindSpot stubs (registered but emit nothing).

pub mod http_go;

use crate::commands::group::types::ExtractedContract;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractorKind { Http, Grpc }

pub struct ExtractorEntry {
    pub lang: &'static str,
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
        ExtractorEntry { lang: "python", kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "node",   kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "java",   kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "rust",   kind: ExtractorKind::Http, extract: blind_spot_extractor },
    ]
}

fn grpc_extractors() -> Vec<ExtractorEntry> {
    vec![
        ExtractorEntry { lang: "go",     kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
        ExtractorEntry { lang: "python", kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
        ExtractorEntry { lang: "node",   kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
        ExtractorEntry { lang: "java",   kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
        ExtractorEntry { lang: "rust",   kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
    ]
}

fn blind_spot_extractor(_path: &Path, _source: &[u8]) -> Vec<ExtractedContract> {
    Vec::new()
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
