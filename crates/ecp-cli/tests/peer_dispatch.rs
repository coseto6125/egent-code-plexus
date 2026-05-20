use chrono::Utc;
use ecp_cli::peer::dispatch::dispatch_peer_dirty_event;
use ecp_core::peer::concern::ImpactCache;
use ecp_core::peer::inbox::{drain, InboxEntry};
use ecp_core::session::overlay::{DirtyEntry, SymbolKind, SymbolRef};
use rustc_hash::FxHashSet;
use tempfile::tempdir;

fn sym(name: &str) -> SymbolRef {
    SymbolRef {
        name: name.into(),
        kind: SymbolKind::Function,
        file: "src/a.rs".into(),
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
    }
}

#[test]
fn hard_dispatches_event() {
    let dir = tempdir().unwrap();
    let receiver_dir = dir.path().to_path_buf();
    let inbox = receiver_dir.join("inbox.jsonl");

    let peer_entry = entry_with(vec![sym("verify_token")]);
    let my_dirty = vec![sym("verify_token")];
    let cache = ImpactCache::from_set(FxHashSet::default());

    dispatch_peer_dirty_event(
        &receiver_dir,
        "abc12",
        1234,
        &Utc::now().to_rfc3339(),
        &peer_entry,
        &my_dirty,
        &cache,
    )
    .unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(matches!(&entries[0], InboxEntry::DirtyEvent { .. }));
}

#[test]
fn soft_dispatches_event() {
    let dir = tempdir().unwrap();
    let receiver_dir = dir.path().to_path_buf();
    let inbox = receiver_dir.join("inbox.jsonl");

    let peer_entry = entry_with(vec![sym("login_handler")]);
    let my_dirty = vec![sym("verify_token")];
    let mut impacted = FxHashSet::default();
    impacted.insert("login_handler".to_string());
    let cache = ImpactCache::from_set(impacted);

    dispatch_peer_dirty_event(
        &receiver_dir,
        "abc12",
        1234,
        &Utc::now().to_rfc3339(),
        &peer_entry,
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

    let peer_entry = entry_with(vec![sym("unrelated")]);
    let my_dirty = vec![sym("verify_token")];
    let cache = ImpactCache::from_set(FxHashSet::default());

    dispatch_peer_dirty_event(
        &receiver_dir,
        "abc12",
        1234,
        &Utc::now().to_rfc3339(),
        &peer_entry,
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

    let peer_entry = entry_with(vec![]); // peer parse_failed scenario
    let my_dirty = vec![sym("foo")];
    let cache = ImpactCache::from_set(FxHashSet::default());

    dispatch_peer_dirty_event(
        &receiver_dir,
        "abc12",
        1234,
        &Utc::now().to_rfc3339(),
        &peer_entry,
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
