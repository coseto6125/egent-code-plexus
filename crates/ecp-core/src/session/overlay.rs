use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyFiles {
    pub version: u32,
    #[serde(default)]
    pub entries: BTreeMap<String, DirtyEntry>,
    /// `BUILDER_FINGERPRINT` of the binary that parsed the fragments. A
    /// manifest without one, or with another, holds parse output of other
    /// rules: its fragments are stale, and the next refresh re-parses every
    /// dirty file into a fresh manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyEntry {
    pub mtime_ns: u64,
    pub content_hash: String,
    pub fragment_id: String,
    pub tantivy_delta_segment: Option<String>,
    #[serde(default)]
    pub parse_failed: bool,
    #[serde(default)]
    pub dirty_symbols: Vec<SymbolRef>,
    /// Fragment bin encoding version. `1` (the serde default, so pre-field
    /// manifests parse unchanged) = `Vec<Fragment>` symbol list; `2` =
    /// `FragmentFile` (symbols + imports + per-symbol calls) consumed by the
    /// query-time `OverlayView`; `3` adds the file's export aliases; `4` adds
    /// each symbol's heritage and enum-variant references; `5` preserves all
    /// field reads and function return types. Readers MUST
    /// dispatch on this — decoding a bin as another version is
    /// undefined under rkyv.
    #[serde(default = "default_fragment_format")]
    pub format: u32,
}

/// Serde default for `DirtyEntry::format`: entries written before the field
/// existed are v1 `Vec<Fragment>` bins.
fn default_fragment_format() -> u32 {
    1
}

/// Fragment bins with symbols, imports and calls, without export aliases.
pub const FRAGMENT_FORMAT_V2: u32 = 2;
/// Fragment bins with export aliases, without heritage and variant refs.
pub const FRAGMENT_FORMAT_V3: u32 = 3;
/// v4 adds heritage and enum-variant references.
pub const FRAGMENT_FORMAT_V4: u32 = 4;
/// Current fragment bin encoding version written by `overlay_writer`; it
/// persists every `RawNode.field_reads` entry and function return type.
pub const FRAGMENT_FORMAT_V5: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRef {
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Const,
    Type,
    Module,
    /// Fallback when the analyzer returns a kind string we don't map (cross-language tail).
    Unknown,
}

impl DirtyFiles {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        atomic_write_json(path, value)
    }
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
    /// An empty manifest of the running binary.
    pub fn empty() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
            builder_fingerprint: Some(crate::registry::BUILDER_FINGERPRINT.to_string()),
        }
    }

    /// Whether the running binary's parser wrote these fragments.
    pub fn from_current_builder(&self) -> bool {
        self.builder_fingerprint.as_deref() == Some(crate::registry::BUILDER_FINGERPRINT)
    }
}
