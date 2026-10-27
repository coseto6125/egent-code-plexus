//! Contract (C5 `RouteKey`, FU-2026-10-25-f21cfea445d0): an L1 fragment id is
//! a hash of the path's route key and the file's content. One content under
//! two routes gets two bins; one content under one route gets one bin, so the
//! retire pass and `admin gc` can rely on a shared bin. An omitted file hashes
//! its real content, not the zero hash its graph carries.

use ecp_cli::session::overlay_reader::view_inputs_under;
use ecp_cli::session::overlay_writer::{
    write_dirty_events, write_dirty_fragments_batch, FragmentInput,
};
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::routing::SizeCap;
use ecp_core::session::{layout, DirtyEvents, SessionMeta, FRAGMENT_FORMAT_V5};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn make_session_dir(tmp: &Path, sid: &str, worktree: &Path) -> PathBuf {
    let session_dir = tmp.join("sessions").join(sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: None,
        started_at: "2026-10-25T10:00:00Z".into(),
        last_touched: "2026-10-25T10:00:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: worktree.to_string_lossy().into_owned(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &sm).unwrap();
    DirtyEvents::write_atomic(
        &layout::dirty_events_path(&session_dir),
        &DirtyEvents::empty(SizeCap::BUILT_IN),
    )
    .unwrap();
    session_dir
}

fn input(rel_path: &str, content: &str) -> FragmentInput {
    FragmentInput {
        rel_path: rel_path.into(),
        content: content.as_bytes().to_vec(),
        mtime_ns: 1,
    }
}

fn bin_count(session_dir: &Path) -> usize {
    std::fs::read_dir(layout::fragments_dir(session_dir))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "bin"))
        .count()
}

/// Contract: a `deploy.yaml` (generic or Kubernetes YAML) and a GitHub
/// workflow with identical bytes parse under different providers, so they must
/// not share a bin.
#[test]
fn test_fragment_key_deploy_yaml_vs_workflow_yaml_two_bins() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "yaml", work.path());
    let content = "name: deploy\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n";

    let outcomes = write_dirty_fragments_batch(
        &sd,
        &[
            input("deploy.yaml", content),
            input(".github/workflows/deploy.yaml", content),
        ],
    )
    .unwrap();

    assert_ne!(
        outcomes[0].fragment_id, outcomes[1].fragment_id,
        "one content under two routes must get two fragment ids"
    );
    assert_eq!(bin_count(&sd), 2, "two ids name two bins");
    let manifest = DirtyEvents::read(&layout::dirty_events_path(&sd)).unwrap();
    assert_eq!(
        manifest.entries["deploy.yaml"].fragment_id.as_deref(),
        Some(outcomes[0].fragment_id.as_str())
    );
    assert_eq!(
        manifest.entries[".github/workflows/deploy.yaml"]
            .fragment_id
            .as_deref(),
        Some(outcomes[1].fragment_id.as_str())
    );
}

/// Contract: the pre-parsed route keys by route too. A graph writer that
/// hashed the content alone would give the two YAML paths one id.
#[test]
fn test_fragment_key_from_graphs_deploy_yaml_vs_workflow_yaml_two_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "yaml-graphs", work.path());
    let content = "name: deploy\non: push\n";
    let pairs: Vec<_> = ["deploy.yaml", ".github/workflows/deploy.yaml"]
        .iter()
        .map(|rel| {
            let abs = work.path().join(rel);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(&abs, content).unwrap();
            (abs, PathBuf::from(rel))
        })
        .collect();
    let graphs = ecp_cli::reanalyze::pipeline().analyze(pairs);
    assert_eq!(graphs.len(), 2);

    let outcomes = write_dirty_events(
        &sd,
        SizeCap::BUILT_IN,
        &graphs,
        &[1, 1],
        &HashMap::new(),
        &[],
    )
    .unwrap();

    assert_ne!(outcomes[0].fragment_id, outcomes[1].fragment_id);
}

/// Contract: an omitted graph carries the zero content hash on purpose; the
/// fragment key hashes the file's real content, so two omitted files with
/// different content get two ids (and a file with the same content as another
/// under one route shares an id).
#[test]
fn test_fragment_key_omitted_files_distinct_content_distinct_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "omitted", work.path());
    for (name, body) in [
        (
            "big_a.ts",
            "export const a = 1; // padding padding padding\n",
        ),
        ("big_b.ts", "export const b = 2; // other padding padding\n"),
        (
            "big_c.ts",
            "export const a = 1; // padding padding padding\n",
        ),
    ] {
        std::fs::write(work.path().join(name), body).unwrap();
    }
    let graphs: Vec<LocalGraph> = ["big_a.ts", "big_b.ts", "big_c.ts"]
        .iter()
        .map(|name| LocalGraph::omitted(Path::new(name), "over-size-cap", "test".into()))
        .collect();
    assert!(graphs.iter().all(|g| g.content_hash == [0u8; 8]));

    let outcomes = write_dirty_events(
        &sd,
        SizeCap::BUILT_IN,
        &graphs,
        &[1, 1, 1],
        &HashMap::new(),
        &[],
    )
    .unwrap();

    assert_ne!(
        outcomes[0].fragment_id, outcomes[1].fragment_id,
        "different content must not share the zero-hash id"
    );
    assert_ne!(outcomes[0].fragment_id, format!("{:016x}", 0u64));
    assert_eq!(
        outcomes[0].fragment_id, outcomes[2].fragment_id,
        "equal content under one route shares an id"
    );
}

/// Edge: an omitted file that is gone from the worktree still gets an id and
/// does not abort the batch.
#[test]
fn test_fragment_key_omitted_file_absent_from_worktree_still_written() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "omitted-gone", work.path());
    let graphs = vec![LocalGraph::omitted(
        Path::new("gone.ts"),
        "file-unreadable",
        "test".into(),
    )];

    let outcomes =
        write_dirty_events(&sd, SizeCap::BUILT_IN, &graphs, &[1], &HashMap::new(), &[]).unwrap();

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].fragment_id.len(), 16);
}

/// Contract: two paths, one route, one content give one id and one bin (the
/// retire pass and `admin gc` rely on a shared bin).
#[test]
fn test_fragment_key_shared_bin_two_paths_one_route_one_id() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "shared", work.path());

    let outcomes = write_dirty_fragments_batch(
        &sd,
        &[
            input("src/a.rs", "fn same() {}\n"),
            input("lib/deep/b.rs", "fn same() {}\n"),
        ],
    )
    .unwrap();

    assert_eq!(outcomes[0].fragment_id, outcomes[1].fragment_id);
    assert_eq!(bin_count(&sd), 1);
    let manifest = DirtyEvents::read(&layout::dirty_events_path(&sd)).unwrap();
    assert_eq!(
        manifest.entries.len(),
        2,
        "both paths keep a manifest entry"
    );
}

/// Edge: concurrent duplicate. The same input twice in one batch must not
/// collide on the tmp file and must give one id.
#[test]
fn test_fragment_key_duplicate_input_in_one_batch_one_id() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "dup", work.path());
    let same = input("src/a.py", "def f():\n    return 1\n");

    let outcomes =
        write_dirty_fragments_batch(&sd, &[same, input("src/a.py", "def f():\n    return 1\n")])
            .unwrap();

    assert_eq!(outcomes[0].fragment_id, outcomes[1].fragment_id);
    assert_eq!(bin_count(&sd), 1);
}

/// Edge: empty and whitespace-only content are valid inputs. Each gets an id,
/// and the two differ (different bytes).
#[test]
fn test_fragment_key_empty_and_whitespace_content_distinct_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "blank", work.path());

    let outcomes =
        write_dirty_fragments_batch(&sd, &[input("empty.py", ""), input("blank.py", "  \n\t\n")])
            .unwrap();

    assert_eq!(outcomes.len(), 2);
    assert_ne!(outcomes[0].fragment_id, outcomes[1].fragment_id);
}

/// Edge: a tombstone has no bin and no id.
#[test]
fn test_fragment_key_tombstone_has_no_fragment_id() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "tomb", work.path());

    write_dirty_events(
        &sd,
        SizeCap::BUILT_IN,
        &[],
        &[],
        &HashMap::new(),
        &[(
            "gone.rs".to_string(),
            ecp_core::contracts::dirty_event::PathClass::SOURCE,
        )],
    )
    .unwrap();

    let manifest = DirtyEvents::read(&layout::dirty_events_path(&sd)).unwrap();
    assert_eq!(manifest.entries["gone.rs"].fragment_id, None);
    assert_eq!(bin_count(&sd), 0);
}

/// Machine-parsed old data: a manifest entry stamped by the V5 binary keys a
/// bin by content alone. The reader must not serve it; the freshness gate
/// re-parses it. Control: the same entry at the writer's current format is
/// served.
#[test]
fn test_view_inputs_v5_entry_is_rejected_not_served() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let sd = make_session_dir(tmp.path(), "v5", work.path());
    let abs = work.path().join("a.rs");
    std::fs::write(&abs, "fn a() {}\n").unwrap();
    let mtime_ns = std::fs::metadata(&abs)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    write_dirty_fragments_batch(
        &sd,
        &[FragmentInput {
            rel_path: "a.rs".into(),
            content: b"fn a() {}\n".to_vec(),
            mtime_ns,
        }],
    )
    .unwrap();

    let served = view_inputs_under(&sd, work.path(), SizeCap::BUILT_IN).unwrap();
    assert_eq!(
        served.fragments.len(),
        1,
        "control: the current entry is served"
    );

    let manifest_path = layout::dirty_events_path(&sd);
    let mut manifest = DirtyEvents::read(&manifest_path).unwrap();
    manifest.entries.get_mut("a.rs").unwrap().format = FRAGMENT_FORMAT_V5;
    DirtyEvents::write_atomic(&manifest_path, &manifest).unwrap();

    let after = view_inputs_under(&sd, work.path(), SizeCap::BUILT_IN).unwrap();
    assert!(
        after.fragments.is_empty(),
        "a V5 entry keys its bin by content alone and must not be served"
    );
}

/// Contract per language: one content at two paths of one route writes the
/// same bin bytes, read from two separate sessions so the equality is not the
/// trivial one of a single shared file. A language whose bytes differ keys its
/// fragment by path.
fn assert_two_paths_one_route_same_bytes(path_a: &str, path_b: &str, content: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let write = |sid: &str, rel: &str| {
        let sd = make_session_dir(tmp.path(), sid, work.path());
        let out = write_dirty_fragments_batch(&sd, &[input(rel, content)]).unwrap();
        assert!(!out[0].parse_failed, "{rel} must parse");
        let bytes = std::fs::read(layout::fragment_path(&sd, &out[0].fragment_id)).unwrap();
        (out[0].fragment_id.clone(), bytes)
    };
    let (id_a, bytes_a) = write("a", path_a);
    let (id_b, bytes_b) = write("b", path_b);
    assert_eq!(id_a, id_b, "{path_a} and {path_b} share one route");
    assert_eq!(
        bytes_a, bytes_b,
        "{path_a} and {path_b} must write equal bin bytes"
    );
}

macro_rules! two_paths_one_route {
    ($($name:ident: $a:literal, $b:literal, $content:literal;)*) => {$(
        #[test]
        fn $name() {
            assert_two_paths_one_route_same_bytes($a, $b, $content);
        }
    )*};
}

two_paths_one_route! {
    test_fragment_bytes_typescript_two_paths_one_route_same_bytes:
        "src/a.ts", "lib/deep/b.ts", "export function f(): number { return 1; }\n";
    test_fragment_bytes_javascript_two_paths_one_route_same_bytes:
        "src/a.js", "lib/deep/b.js", "export function f() { return 1; }\n";
    test_fragment_bytes_python_two_paths_one_route_same_bytes:
        "src/a.py", "lib/deep/b.py", "def f():\n    return 1\n";
    test_fragment_bytes_java_two_paths_one_route_same_bytes:
        "src/A.java", "lib/deep/B.java", "class K { int f() { return 1; } }\n";
    test_fragment_bytes_kotlin_two_paths_one_route_same_bytes:
        "src/a.kt", "lib/deep/b.kt", "fun f(): Int { return 1 }\n";
    test_fragment_bytes_csharp_two_paths_one_route_same_bytes:
        "src/A.cs", "lib/deep/B.cs", "class K { int F() { return 1; } }\n";
    test_fragment_bytes_go_two_paths_one_route_same_bytes:
        "src/a.go", "lib/deep/b.go", "package p\n\nfunc F() int { return 1 }\n";
    test_fragment_bytes_rust_two_paths_one_route_same_bytes:
        "src/a.rs", "lib/deep/b.rs", "pub fn f() -> i32 { 1 }\n";
    test_fragment_bytes_php_two_paths_one_route_same_bytes:
        "src/a.php", "lib/deep/b.php", "<?php\nfunction f() { return 1; }\n";
    test_fragment_bytes_ruby_two_paths_one_route_same_bytes:
        "src/a.rb", "lib/deep/b.rb", "def f\n  1\nend\n";
    test_fragment_bytes_swift_two_paths_one_route_same_bytes:
        "src/a.swift", "lib/deep/b.swift", "func f() -> Int { return 1 }\n";
    test_fragment_bytes_c_two_paths_one_route_same_bytes:
        "src/a.c", "lib/deep/b.c", "int f(void) { return 1; }\n";
    test_fragment_bytes_cpp_two_paths_one_route_same_bytes:
        "src/a.cpp", "lib/deep/b.cpp", "int f() { return 1; }\n";
    test_fragment_bytes_dart_two_paths_one_route_same_bytes:
        "src/a.dart", "lib/deep/b.dart", "int f() { return 1; }\n";
}
