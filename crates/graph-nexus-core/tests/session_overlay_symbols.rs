use graph_nexus_core::session::overlay::{DirtyEntry, DirtyFiles, SymbolKind, SymbolRef};
use std::collections::BTreeMap;

#[test]
fn dirty_entry_serialises_dirty_symbols() {
    let entry = DirtyEntry {
        mtime_ns: 1,
        content_hash: "h".into(),
        fragment_id: "f".into(),
        tantivy_delta_segment: None,
        parse_failed: false,
        dirty_symbols: vec![SymbolRef {
            name: "verify_token".into(),
            kind: SymbolKind::Function,
            file: "src/auth.rs".into(),
            line_start: 42,
            line_end: 58,
        }],
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"name\":\"verify_token\""));
    assert!(json.contains("\"kind\":\"function\""));
    let back: DirtyEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.dirty_symbols.len(), 1);
    assert_eq!(back.dirty_symbols[0].line_start, 42);
}

#[test]
fn dirty_entry_deserialises_without_dirty_symbols_field() {
    let legacy = r#"{
        "mtime_ns":1,"content_hash":"h","fragment_id":"f",
        "tantivy_delta_segment":null,"parse_failed":false
    }"#;
    let entry: DirtyEntry = serde_json::from_str(legacy).unwrap();
    assert!(entry.dirty_symbols.is_empty());
}

#[test]
fn dirty_files_round_trip_via_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dirty.json");
    let mut entries = BTreeMap::new();
    entries.insert("src/a.rs".to_string(), DirtyEntry {
        mtime_ns: 1, content_hash: "h".into(), fragment_id: "f".into(),
        tantivy_delta_segment: None, parse_failed: false,
        dirty_symbols: vec![SymbolRef {
            name: "foo".into(), kind: SymbolKind::Function,
            file: "src/a.rs".into(), line_start: 1, line_end: 10,
        }],
    });
    let files = DirtyFiles { version: 1, entries };
    DirtyFiles::write_atomic(&path, &files).unwrap();
    let back = DirtyFiles::read(&path).unwrap();
    assert_eq!(back.entries["src/a.rs"].dirty_symbols[0].name, "foo");
}
