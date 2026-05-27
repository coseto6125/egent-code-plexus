use ecp_cli::session::overlay_writer::{
    write_dirty_fragment, write_dirty_fragments_batch, write_dirty_fragments_from_graphs,
    FragmentInput,
};
use ecp_core::session::{DirtyFiles, SessionMeta};

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
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &sm).unwrap();
    let df = DirtyFiles::empty();
    DirtyFiles::write_atomic(&session_dir.join("dirty_files.json"), &df).unwrap();
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
    let frags: Vec<_> = std::fs::read_dir(session_dir.join("graph_overlay"))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(frags.len(), 1);

    // Manifest updated with this file
    let df = DirtyFiles::read(&session_dir.join("dirty_files.json")).unwrap();
    assert!(df.entries.contains_key("src/a.rs"));
    let entry = &df.entries["src/a.rs"];
    assert_eq!(entry.mtime_ns, 1000);
    assert!(!entry.parse_failed);
    assert_eq!(entry.fragment_id, outcome.fragment_id);
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

    let df = DirtyFiles::read(&session_dir.join("dirty_files.json")).unwrap();
    assert!(df.entries.contains_key("a.rs"));
    assert!(df.entries.contains_key("b.rs"));

    let sm = SessionMeta::read(&session_dir.join("session_meta.json")).unwrap();
    assert_eq!(sm.overlay_version, 2);

    let overlay_dir = session_dir.join("graph_overlay");
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
    let frag = |sd: &std::path::Path, id: &str| {
        std::fs::read(sd.join("graph_overlay").join(format!("{id}.bin"))).unwrap()
    };
    assert_eq!(
        frag(&sd_a, &reparse[0].fragment_id),
        frag(&sd_b, &preparsed[0].fragment_id),
        "fragment archive bytes must match between the two routes"
    );
}

/// The dispatch table extracted from `find_provider` must keep resolving the
/// extensions the reanalyze subset relies on.
#[test]
fn provider_name_dispatch_covers_common_extensions() {
    use ecp_core::analyzer::pipeline::AnalyzerPipeline as P;
    use std::path::Path;
    assert_eq!(
        P::provider_name_for_path(Path::new("a.ts")),
        Some("typescript")
    );
    assert_eq!(P::provider_name_for_path(Path::new("a.rs")), Some("rust"));
    assert_eq!(P::provider_name_for_path(Path::new("a.py")), Some("python"));
    assert_eq!(P::provider_name_for_path(Path::new("a.h")), Some("cpp"));
    assert_eq!(
        P::provider_name_for_path(Path::new("Dockerfile")),
        Some("dockerfile")
    );
    assert_eq!(
        P::provider_name_for_path(Path::new(".github/workflows/ci.yml")),
        Some("github-actions")
    );
    assert_eq!(P::provider_name_for_path(Path::new("a.unknownext")), None);
}
