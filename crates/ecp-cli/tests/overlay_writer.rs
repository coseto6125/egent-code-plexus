use ecp_cli::session::overlay_writer::{
    retire_entries, write_dirty_events, write_dirty_fragment, write_dirty_fragments_batch,
    write_dirty_fragments_from_graphs, DirtyPathEvent, FragmentInput,
};
use ecp_core::contracts::dirty_event::{PathChange, PathClass};
use ecp_core::contracts::routing::SizeCap;
use ecp_core::session::{layout, DirtyEventEntry, DirtyEvents, SessionMeta};
use std::collections::HashMap;

fn make_session_dir(tmp: &std::path::Path, sid: &str) -> std::path::PathBuf {
    let session_dir = tmp.join("sessions").join(sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: "/work/x".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &sm).unwrap();
    let df = DirtyEvents::empty(SizeCap::BUILT_IN);
    DirtyEvents::write_atomic(&layout::dirty_events_path(&session_dir), &df).unwrap();
    session_dir
}

#[test]
fn first_dirty_file_creates_fragment_and_manifest_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "test-sid");

    let input = FragmentInput {
        rel_path: "src/a.rs".into(),
        content: b"fn a() {}".to_vec(),
        mtime_ns: 1000,
    };
    let outcome = write_dirty_fragment(&session_dir, &input).unwrap();
    assert!(!outcome.parse_failed);

    // Fragment file exists
    let frags: Vec<_> = std::fs::read_dir(layout::fragments_dir(&session_dir))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(frags.len(), 1);

    // Manifest updated with this file
    let df = DirtyEvents::read(&layout::dirty_events_path(&session_dir)).unwrap();
    assert!(df.entries.contains_key("src/a.rs"));
    let entry = &df.entries["src/a.rs"];
    assert_eq!(entry.mtime_ns, 1000);
    assert!(!entry.parse_failed);
    assert_eq!(
        entry.fragment_id.as_deref(),
        Some(outcome.fragment_id.as_str())
    );
}

#[test]
fn overlay_version_bumps_on_each_write() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "v-sid");

    let input1 = FragmentInput {
        rel_path: "a.rs".into(),
        content: b"x".to_vec(),
        mtime_ns: 1,
    };
    write_dirty_fragment(&session_dir, &input1).unwrap();
    let sm1 = SessionMeta::read(&session_dir.join("session_meta.json")).unwrap();
    assert_eq!(sm1.overlay_version, 1);

    let input2 = FragmentInput {
        rel_path: "b.rs".into(),
        content: b"y".to_vec(),
        mtime_ns: 2,
    };
    write_dirty_fragment(&session_dir, &input2).unwrap();
    let sm2 = SessionMeta::read(&session_dir.join("session_meta.json")).unwrap();
    assert_eq!(sm2.overlay_version, 2);
}

#[test]
fn content_hash_drives_fragment_id() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "h-sid");

    let input_a = FragmentInput {
        rel_path: "a.rs".into(),
        content: b"hello".to_vec(),
        mtime_ns: 1,
    };
    let out_a = write_dirty_fragment(&session_dir, &input_a).unwrap();

    let input_b = FragmentInput {
        rel_path: "b.rs".into(),
        content: b"hello".to_vec(), // same content
        mtime_ns: 2,
    };
    let out_b = write_dirty_fragment(&session_dir, &input_b).unwrap();

    // Same content → same fragment_id (the file maps to its content's hash)
    assert_eq!(out_a.fragment_id, out_b.fragment_id);
}

#[test]
fn same_file_re_written_same_content_idempotent_id() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "i-sid");

    let input = FragmentInput {
        rel_path: "a.rs".into(),
        content: b"same".to_vec(),
        mtime_ns: 1,
    };
    let out1 = write_dirty_fragment(&session_dir, &input).unwrap();
    let out2 = write_dirty_fragment(&session_dir, &input).unwrap();
    assert_eq!(out1.fragment_id, out2.fragment_id);
}

#[test]
fn batch_write_same_content_inputs_do_not_collide_on_tmp_files() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "batch-sid");

    let inputs = vec![
        FragmentInput {
            rel_path: "a.rs".into(),
            content: b"same".to_vec(),
            mtime_ns: 1,
        },
        FragmentInput {
            rel_path: "b.rs".into(),
            content: b"same".to_vec(),
            mtime_ns: 2,
        },
    ];

    let outcomes = write_dirty_fragments_batch(&session_dir, &inputs).unwrap();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].fragment_id, outcomes[1].fragment_id);

    let df = DirtyEvents::read(&layout::dirty_events_path(&session_dir)).unwrap();
    assert!(df.entries.contains_key("a.rs"));
    assert!(df.entries.contains_key("b.rs"));

    let sm = SessionMeta::read(&session_dir.join("session_meta.json")).unwrap();
    assert_eq!(sm.overlay_version, 2);

    let overlay_dir = layout::fragments_dir(&session_dir);
    let frag_count = std::fs::read_dir(&overlay_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("bin"))
        .count();
    assert_eq!(frag_count, 1);
}

/// Root-fix regression: feeding `write_dirty_fragments_from_graphs` a
/// pre-parsed `LocalGraph` must produce a fragment byte-identical to the
/// re-parse route (`write_dirty_fragments_batch`) for the same source — the
/// content hash and node set are derived the same way, so the only difference
/// the optimisation introduces is *who parsed*, never *what was written*.
#[test]
fn from_graphs_matches_reparse_route_fragment_id() {
    let src = b"export function hello() { return 1; }\n";
    let rel = "src/x.ts";

    // Parse the source the way `reanalyze_files` does: write to a tempdir and
    // run the pipeline's `analyze`, which sets `content_hash`.
    let work = tempfile::tempdir().unwrap();
    let abs = work.path().join(rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();
    let graphs =
        ecp_cli::reanalyze::pipeline().analyze(vec![(abs.clone(), std::path::PathBuf::from(rel))]);
    assert_eq!(graphs.len(), 1, "ts source must produce exactly one graph");

    // Re-parse route.
    let tmp_a = tempfile::tempdir().unwrap();
    let sd_a = make_session_dir(tmp_a.path(), "sid-a");
    let reparse = write_dirty_fragments_batch(
        &sd_a,
        &[FragmentInput {
            rel_path: rel.into(),
            content: src.to_vec(),
            mtime_ns: 42,
        }],
    )
    .unwrap();

    // Pre-parsed route.
    let tmp_b = tempfile::tempdir().unwrap();
    let sd_b = make_session_dir(tmp_b.path(), "sid-b");
    let preparsed = write_dirty_fragments_from_graphs(&sd_b, &graphs, &[42]).unwrap();

    assert_eq!(reparse.len(), 1);
    assert_eq!(preparsed.len(), 1);
    assert!(!reparse[0].parse_failed && !preparsed[0].parse_failed);
    assert_eq!(
        reparse[0].fragment_id, preparsed[0].fragment_id,
        "pre-parsed route must reproduce the re-parse route's content-hash fragment id"
    );

    // The on-disk fragment archives must be byte-identical.
    let frag =
        |sd: &std::path::Path, id: &str| std::fs::read(layout::fragment_path(sd, id)).unwrap();
    assert_eq!(
        frag(&sd_a, &reparse[0].fragment_id),
        frag(&sd_b, &preparsed[0].fragment_id),
        "fragment archive bytes must match between the two routes"
    );
}

/// The overlay's per-path pipeline exists for exactly the paths a route
/// lists, and the route's first provider is the one the reanalyze subset
/// relies on, for common extensions and path rules.
#[test]
fn provider_dispatch_covers_common_extensions() {
    use ecp_cli::reanalyze::pipeline_for_path;
    use ecp_core::contracts::routing::{FileRouter, SizeCap};
    use std::path::Path;
    let router = FileRouter::PATH_ONLY;
    for (rel, first) in [
        ("a.ts", Some("typescript")),
        ("a.rs", Some("rust")),
        ("a.py", Some("python")),
        ("a.h", Some("cpp")),
        ("a.sh", Some("bash")),
        ("Dockerfile", Some("dockerfile")),
        (".github/workflows/ci.yml", Some("github-actions")),
        ("a.unknownext", None),
    ] {
        let routed = router
            .route(rel)
            .route()
            .and_then(|route| route.providers().next());
        assert_eq!(routed, first, "{rel}");
        assert_eq!(
            pipeline_for_path(Path::new(rel), SizeCap::BUILT_IN).is_some(),
            first.is_some(),
            "{rel}"
        );
    }
}

fn parse_ts_files(names: &[&str]) -> Vec<ecp_core::analyzer::types::LocalGraph> {
    let work = tempfile::tempdir().unwrap();
    let pairs = names
        .iter()
        .map(|name| {
            let abs = work.path().join(name);
            std::fs::write(
                &abs,
                format!("export function f_{}() {{ return 1; }}\n", &name[..1]),
            )
            .unwrap();
            (abs, std::path::PathBuf::from(name))
        })
        .collect();
    ecp_cli::reanalyze::pipeline().analyze(pairs)
}

/// Contract: the manifest records the cap the caller parsed under, so a reader
/// under another cap rejects the fragments instead of serving them.
#[test]
fn test_write_dirty_events_non_default_cap_manifest_stamps_given_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "sid-cap");
    let cap = SizeCap::resolve(None, Some(64));
    assert_ne!(cap.bytes(), SizeCap::BUILT_IN.bytes());
    let graphs = parse_ts_files(&["a.ts"]);

    write_dirty_events(&sd, cap, &graphs, &[7], &HashMap::new(), &[]).unwrap();

    let manifest = DirtyEvents::read(&layout::dirty_events_path(&sd)).unwrap();
    assert_eq!(manifest.size_cap_bytes, cap.bytes());
    assert!(manifest.is_current(cap));
    assert!(!manifest.is_current(SizeCap::BUILT_IN));
}

/// Contract: the manifest stores confirmed changes only. `unmerged` and a
/// `deleted` path that was parsed (it exists again) become `modified`;
/// `added` stays `added`; none is ever `unmerged`.
#[test]
fn test_write_dirty_events_unmerged_deleted_added_store_parsed_change() {
    let tmp = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "sid-parsed");
    let graphs = parse_ts_files(&["a.ts", "b.ts", "c.ts"]);
    let event = |change| DirtyPathEvent {
        change,
        class: PathClass::SOURCE,
    };
    let events = HashMap::from([
        ("a.ts".to_string(), event(PathChange::Unmerged)),
        ("b.ts".to_string(), event(PathChange::Deleted)),
        ("c.ts".to_string(), event(PathChange::Added)),
    ]);

    write_dirty_events(&sd, SizeCap::BUILT_IN, &graphs, &[1, 2, 3], &events, &[]).unwrap();

    let manifest = DirtyEvents::read(&layout::dirty_events_path(&sd)).unwrap();
    let change = |p: &str| manifest.entries[p].change;
    assert_eq!(change("a.ts"), PathChange::Modified);
    assert_eq!(change("b.ts"), PathChange::Modified);
    assert_eq!(change("c.ts"), PathChange::Added);
    assert!(manifest
        .entries
        .values()
        .all(|e| e.change != PathChange::Unmerged));
}

fn write_two(session_dir: &std::path::Path) {
    for (rel, content) in [("src/a.rs", "fn a() {}"), ("src/b.rs", "fn b() {}")] {
        let input = FragmentInput {
            rel_path: rel.into(),
            content: content.as_bytes().to_vec(),
            mtime_ns: 1000,
        };
        write_dirty_fragment(session_dir, &input).unwrap();
    }
}

/// The entries the manifest holds now, as a caller snapshots them.
fn snapshot(session_dir: &std::path::Path, rels: &[&str]) -> HashMap<String, DirtyEventEntry> {
    let df = DirtyEvents::read(&layout::dirty_events_path(session_dir)).unwrap();
    rels.iter()
        .map(|rel| (rel.to_string(), df.entries[*rel].clone()))
        .collect()
}

#[test]
fn test_retire_entries_named_entry_drops_it_and_keeps_bin() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "retire");
    write_two(&session_dir);
    let stale = snapshot(&session_dir, &["src/a.rs"]);
    let bin = stale["src/a.rs"].fragment_id.clone().unwrap();

    assert_eq!(
        retire_entries(&session_dir, &stale, |_, _| true).unwrap(),
        1
    );

    let df = DirtyEvents::read(&layout::dirty_events_path(&session_dir)).unwrap();
    assert_eq!(df.entries.keys().collect::<Vec<_>>(), ["src/b.rs"]);
    assert!(
        layout::fragment_path(&session_dir, &bin).exists(),
        "a bin can be shared by content hash; admin gc owns its removal"
    );
}

#[test]
fn test_retire_entries_same_content_rewritten_since_stays() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "retire-race");
    write_two(&session_dir);
    let stale = snapshot(&session_dir, &["src/a.rs"]);
    // Another process re-publishes the same content after a new write: the
    // fragment id (a content hash) is unchanged, the pre-parse mtime is not.
    let input = FragmentInput {
        rel_path: "src/a.rs".into(),
        content: b"fn a() {}".to_vec(),
        mtime_ns: 2000,
    };
    write_dirty_fragment(&session_dir, &input).unwrap();

    assert_eq!(
        retire_entries(&session_dir, &stale, |_, _| true).unwrap(),
        0
    );
    let df = DirtyEvents::read(&layout::dirty_events_path(&session_dir)).unwrap();
    assert_eq!(df.entries["src/a.rs"].mtime_ns, 2000);
    assert_eq!(
        df.entries["src/a.rs"].fragment_id,
        stale["src/a.rs"].fragment_id
    );
}

#[test]
fn test_retire_entries_last_entry_removes_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "retire-all");
    write_two(&session_dir);
    let stale = snapshot(&session_dir, &["src/a.rs", "src/b.rs"]);

    assert_eq!(
        retire_entries(&session_dir, &stale, |_, _| true).unwrap(),
        2
    );
    assert!(!layout::dirty_events_path(&session_dir).exists());
    assert_eq!(
        retire_entries(&session_dir, &stale, |_, _| true).unwrap(),
        0,
        "a missing manifest has nothing to retire"
    );
}
