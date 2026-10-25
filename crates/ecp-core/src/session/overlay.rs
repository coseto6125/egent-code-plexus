use crate::contracts::dirty_event::{PathChange, PathClass};
use crate::contracts::routing::SizeCap;
use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

/// The legacy dirty manifest, `<session>/dirty_files.json`
/// ([`super::layout::legacy_dirty_files_path`]). An older binary of the same
/// fingerprint still writes it; today's query paths never read it. Kept for
/// `admin gc` and for old-data tests.
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
    /// field reads and function return types; `6` keys the fragment id by
    /// route; `7` adds each function symbol's `RawFunctionMeta.flags` and
    /// the file's raw records (`OverlayFileRecords`). Readers MUST
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
/// v5 persists every `RawNode.field_reads` entry and function return type.
/// Its fragment id hashes the content alone, so one content under two
/// routes shared one bin.
pub const FRAGMENT_FORMAT_V5: u32 = 5;
/// v6 is the v5 bin under a fragment id that hashes the route key and the
/// content.
pub const FRAGMENT_FORMAT_V6: u32 = 6;
/// Current fragment bin encoding version written by `overlay_writer`: v6
/// plus each symbol's `RawFunctionMeta.flags` and the file's raw records
/// (path literals, SQL references, transaction scopes, schema fields, event
/// topics: `OverlayFileRecords`), each endpoint bound. V7 is unreleased, so
/// the records extend it in place: a V7 bin written before them fails the
/// archive check, and the freshness gate re-parses its file.
pub const FRAGMENT_FORMAT_V7: u32 = 7;

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

/// The `format` of a [`DirtyEvents`] manifest this binary reads and writes.
pub const DIRTY_EVENTS_FORMAT: u32 = 1;

/// The dirty manifest, `<session>/l1/dirty_events.json`
/// ([`super::layout::dirty_events_path`]), with its bins in `l1/fragments/`
/// (`docs/specs/contracts/C3-dirty-event.md`, "Manifest isolation").
///
/// No field has a serde default: the namespace holds no older data to
/// default for, and a manifest that fails to parse is re-parsed whole by
/// the freshness gate. A manifest that parses is served only while
/// [`DirtyEvents::is_current`] holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyEvents {
    /// [`DIRTY_EVENTS_FORMAT`] of the writer.
    pub format: u32,
    /// `SizeCap::bytes()` of the cap the fragments were parsed under. A file
    /// over the cap parses to an omission, so a cap change changes the parse.
    pub size_cap_bytes: u64,
    /// `BUILDER_FINGERPRINT` of the binary that parsed the fragments.
    pub builder_fingerprint: String,
    /// Keyed by the worktree-top-relative, `/`-separated path.
    pub entries: BTreeMap<String, DirtyEventEntry>,
}

/// One dirty path of a [`DirtyEvents`] manifest.
///
/// A tombstone (a deleted file, the removed half of a rename) has
/// `change: deleted`, no `fragment_id` and no bin. It stays in the manifest
/// when the file comes back, so a reader honours it only while the path is
/// still absent from the worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyEventEntry {
    /// The confirmed C3 change of the path: never `unmerged`.
    pub change: PathChange,
    /// The C3 class of the path.
    pub class: PathClass,
    /// The file's mtime when it was statted before the parse; `0` for a
    /// tombstone.
    pub mtime_ns: u64,
    /// The bin `l1/fragments/<fragment_id>.bin`; `None` for a tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment_id: Option<String>,
    /// xxh3 of the parsed content; `None` for a tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub parse_failed: bool,
    #[serde(default)]
    pub dirty_symbols: Vec<SymbolRef>,
    /// The fragment bin encoding (`FRAGMENT_FORMAT_V6`). Readers dispatch on
    /// it, as for [`DirtyEntry::format`].
    pub format: u32,
}

impl DirtyEventEntry {
    /// A deleted path: no bin, no symbols.
    pub fn tombstone(class: PathClass) -> Self {
        Self {
            change: PathChange::Deleted,
            class,
            mtime_ns: 0,
            fragment_id: None,
            content_hash: None,
            parse_failed: false,
            dirty_symbols: Vec::new(),
            format: FRAGMENT_FORMAT_V6,
        }
    }

    pub fn is_tombstone(&self) -> bool {
        self.change == PathChange::Deleted && self.fragment_id.is_none()
    }
}

impl DirtyEvents {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write_json(path, value)
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        super::meta::read_json(path)
    }

    /// An empty manifest of the running binary under `size_cap`.
    pub fn empty(size_cap: SizeCap) -> Self {
        Self {
            format: DIRTY_EVENTS_FORMAT,
            size_cap_bytes: size_cap.bytes(),
            builder_fingerprint: crate::registry::BUILDER_FINGERPRINT.to_string(),
            entries: BTreeMap::new(),
        }
    }

    /// Whether the running binary's parser wrote these fragments.
    pub fn from_current_builder(&self) -> bool {
        self.builder_fingerprint == crate::registry::BUILDER_FINGERPRINT
    }

    /// Whether the entries may be served under `size_cap`: this format, this
    /// builder and this cap. Any other manifest makes every dirty file a
    /// re-parse.
    pub fn is_current(&self, size_cap: SizeCap) -> bool {
        self.format == DIRTY_EVENTS_FORMAT
            && self.size_cap_bytes == size_cap.bytes()
            && self.from_current_builder()
    }
}

#[cfg(test)]
mod dirty_events_tests {
    use super::*;

    fn entry_json(extra: &str) -> String {
        format!(r#"{{"mtime_ns":1,"fragment_id":"f","content_hash":"f","format":5{extra}}}"#)
    }

    fn manifest_json(header: &str, entry: &str) -> String {
        format!(r#"{{{header}"builder_fingerprint":"b","entries":{{"a.rs":{entry}}}}}"#)
    }

    #[test]
    fn test_dirty_events_read_missing_format_fails_parse() {
        let json = manifest_json(
            r#""size_cap_bytes":1,"#,
            &entry_json(
                r#","change":"modified","class":{"source":true,"resolution_config":false}"#,
            ),
        );
        assert!(serde_json::from_str::<DirtyEvents>(&json).is_err());
    }

    #[test]
    fn test_dirty_events_read_entry_without_change_or_class_fails_parse() {
        for extra in [
            r#","class":{"source":true,"resolution_config":false}"#,
            r#","change":"modified""#,
        ] {
            let json = manifest_json(r#""format":1,"size_cap_bytes":1,"#, &entry_json(extra));
            assert!(
                serde_json::from_str::<DirtyEvents>(&json).is_err(),
                "{extra}"
            );
        }
    }

    #[test]
    fn test_dirty_events_is_current_other_format_cap_or_builder_false() {
        let cap = SizeCap::BUILT_IN;
        let current = DirtyEvents::empty(cap);
        assert!(current.is_current(cap));
        let mut other_format = current.clone();
        other_format.format = DIRTY_EVENTS_FORMAT + 1;
        let mut other_cap = current.clone();
        other_cap.size_cap_bytes = cap.bytes() + 1;
        let mut other_builder = current.clone();
        other_builder.builder_fingerprint = "v0.0.0+schema0".into();
        for (what, manifest) in [
            ("format", other_format),
            ("cap", other_cap),
            ("builder", other_builder),
        ] {
            assert!(!manifest.is_current(cap), "{what}");
        }
    }

    #[test]
    fn test_dirty_events_tombstone_round_trip_keeps_change_class_and_no_bin() {
        let dir = tempfile::tempdir().unwrap();
        let path = super::super::layout::dirty_events_path(dir.path());
        let mut manifest = DirtyEvents::empty(SizeCap::BUILT_IN);
        manifest.entries.insert(
            "gone.rs".into(),
            DirtyEventEntry::tombstone(PathClass::SOURCE),
        );
        DirtyEvents::write_atomic(&path, &manifest).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let back = DirtyEvents::read(&path).unwrap();

        let row: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(row["entries"]["gone.rs"]["change"], "deleted");
        assert!(
            row["entries"]["gone.rs"].get("fragment_id").is_none(),
            "{text}"
        );
        let entry = &back.entries["gone.rs"];
        assert!(entry.is_tombstone());
        assert_eq!(entry.class, PathClass::SOURCE);
    }
}
