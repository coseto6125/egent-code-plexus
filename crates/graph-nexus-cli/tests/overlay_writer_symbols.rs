use graph_nexus_cli::session::overlay_writer::OverlayWriter;
use std::fs;
use tempfile::tempdir;

#[test]
fn write_dirty_records_function_symbol() {
    let dir = tempdir().unwrap();
    let session_dir = dir.path().join("sessions/s1");
    fs::create_dir_all(&session_dir).unwrap();
    let src_path = dir.path().join("src/lib.rs");
    fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    fs::write(&src_path, "pub fn verify_token() -> bool { true }\n").unwrap();

    let mut writer = OverlayWriter::new(&session_dir);
    writer.append_dirty(&src_path, "deadbeef", "f1").unwrap();

    let dirty = writer.read_dirty().unwrap();
    let entry = dirty.entries.values().next().unwrap();
    assert!(
        entry.dirty_symbols.iter().any(|s| s.name == "verify_token"),
        "expected verify_token in dirty_symbols, got: {:?}",
        entry.dirty_symbols
    );
}

#[test]
fn write_dirty_on_unsupported_file_marks_parse_failed() {
    let dir = tempdir().unwrap();
    let session_dir = dir.path().join("sessions/s1");
    fs::create_dir_all(&session_dir).unwrap();
    let src_path = dir.path().join("README.bin");
    fs::write(&src_path, "binary garbage").unwrap();

    let mut writer = OverlayWriter::new(&session_dir);
    writer.append_dirty(&src_path, "x", "y").unwrap();

    let dirty = writer.read_dirty().unwrap();
    let entry = dirty.entries.values().next().unwrap();
    assert!(entry.dirty_symbols.is_empty());
    assert!(entry.parse_failed);
}
