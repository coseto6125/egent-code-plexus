use chrono::Utc;
use ecp_cli::peer::dispatch::dispatch_peer_dirty_event;
use ecp_core::peer::concern::ImpactCache;
use ecp_core::peer::inbox::{drain, InboxEntry};
use ecp_core::session::overlay::{DirtyEntry, SymbolKind, SymbolRef};
use rustc_hash::FxHashSet;
use tempfile::tempdir;

fn sym(name: &str) -> SymbolRef {
    sym_in(name, "src/a.rs")
}

fn sym_in(name: &str, file: &str) -> SymbolRef {
    SymbolRef {
        name: name.into(),
        kind: SymbolKind::Function,
        file: file.into(),
        line_start: 1,
        line_end: 2,
    }
}

fn entry_with(syms: Vec<SymbolRef>) -> DirtyEntry {
    DirtyEntry {
        mtime_ns: 1,
        content_hash: "h".into(),
        fragment_id: "f".into(),
        tantivy_delta_segment: None,
        parse_failed: false,
        dirty_symbols: syms,
        format: 1,
    }
}

#[test]
fn hard_dispatches_event() {
    let dir = tempdir().unwrap();
    let receiver_dir = dir.path().to_path_buf();
    let inbox = receiver_dir.join("inbox.jsonl");

    let peer_file = "src/a.rs";
    let peer_entry = entry_with(vec![sym("verify_token")]);
    let my_dirty = vec![sym("verify_token")];
    let my_files: Vec<String> = my_dirty.iter().map(|s| s.file.clone()).collect();
    let cache = ImpactCache::from_set(FxHashSet::default());

    dispatch_peer_dirty_event(
        &receiver_dir,
        "abc12",
        1234,
        None,
        &Utc::now().to_rfc3339(),
        peer_file,
        &peer_entry,
        &my_files,
        &my_dirty,
        &cache,
    )
    .unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(matches!(&entries[0], InboxEntry::DirtyEvent { .. }));
}

#[test]
fn dispatch_carries_peer_name_into_entry() {
    let dir = tempdir().unwrap();
    let receiver_dir = dir.path().to_path_buf();
    let inbox = receiver_dir.join("inbox.jsonl");

    let peer_file = "src/a.rs";
    let peer_entry = entry_with(vec![sym("verify_token")]);
    let my_dirty = vec![sym("verify_token")];
    let my_files: Vec<String> = my_dirty.iter().map(|s| s.file.clone()).collect();
    let cache = ImpactCache::from_set(FxHashSet::default());

    dispatch_peer_dirty_event(
        &receiver_dir,
        "abc12",
        1234,
        Some("rust-parser"),
        &Utc::now().to_rfc3339(),
        peer_file,
        &peer_entry,
        &my_files,
        &my_dirty,
        &cache,
    )
    .unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 1);
    match &entries[0] {
        InboxEntry::DirtyEvent { peer_name, .. } => {
            assert_eq!(peer_name.as_deref(), Some("rust-parser"));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn soft_dispatches_event() {
    let dir = tempdir().unwrap();
    let receiver_dir = dir.path().to_path_buf();
    let inbox = receiver_dir.join("inbox.jsonl");

    // SOFT only reachable when the files differ — a shared file is HARD first.
    let peer_file = "src/login.rs";
    let peer_entry = entry_with(vec![sym_in("login_handler", "src/login.rs")]);
    let my_dirty = vec![sym("verify_token")];
    let my_files: Vec<String> = my_dirty.iter().map(|s| s.file.clone()).collect();
    let mut impacted = FxHashSet::default();
    impacted.insert(("src/login.rs".to_string(), "login_handler".to_string()));
    let cache = ImpactCache::from_set(impacted);

    dispatch_peer_dirty_event(
        &receiver_dir,
        "abc12",
        1234,
        None,
        &Utc::now().to_rfc3339(),
        peer_file,
        &peer_entry,
        &my_files,
        &my_dirty,
        &cache,
    )
    .unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 1);
}

#[test]
fn ignore_writes_nothing() {
    let dir = tempdir().unwrap();
    let receiver_dir = dir.path().to_path_buf();
    let inbox = receiver_dir.join("inbox.jsonl");

    // A different FILE, not just a different name: HARD is a shared dirty file,
    // so a peer symbol in `src/a.rs` would be a hit however it is named.
    let peer_file = "src/z.rs";
    let peer_entry = entry_with(vec![sym_in("unrelated", "src/z.rs")]);
    let my_dirty = vec![sym("verify_token")];
    let my_files: Vec<String> = my_dirty.iter().map(|s| s.file.clone()).collect();
    let cache = ImpactCache::from_set(FxHashSet::default());

    dispatch_peer_dirty_event(
        &receiver_dir,
        "abc12",
        1234,
        None,
        &Utc::now().to_rfc3339(),
        peer_file,
        &peer_entry,
        &my_files,
        &my_dirty,
        &cache,
    )
    .unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert!(entries.is_empty());
}

#[test]
fn empty_dirty_symbols_writes_nothing() {
    let dir = tempdir().unwrap();
    let receiver_dir = dir.path().to_path_buf();
    let inbox = receiver_dir.join("inbox.jsonl");

    // A parse-failed peer entry in a file we have NOT touched: no declarations
    // to match and no shared file, so nothing should be written.
    let peer_file = "src/parse_failed.rs";
    let peer_entry = entry_with(vec![]);
    let my_dirty = vec![sym("foo")];
    let my_files: Vec<String> = my_dirty.iter().map(|s| s.file.clone()).collect();
    let cache = ImpactCache::from_set(FxHashSet::default());

    dispatch_peer_dirty_event(
        &receiver_dir,
        "abc12",
        1234,
        None,
        &Utc::now().to_rfc3339(),
        peer_file,
        &peer_entry,
        &my_files,
        &my_dirty,
        &cache,
    )
    .unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert!(entries.is_empty());
}

#[test]
fn watcher_lock_rejects_second_holder() {
    use fs2::FileExt;
    use std::fs::OpenOptions;
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("watcher.lock");
    let f1 = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock)
        .unwrap();
    f1.try_lock_exclusive().unwrap();
    let f2 = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock)
        .unwrap();
    assert!(
        f2.try_lock_exclusive().is_err(),
        "second flock must fail while first holds it"
    );
}
