//! Data the previous release wrote, read back by today's code.
//!
//! `fixtures/stored-by-0.16.4/` holds the files ecp 0.16.4 wrote to `~/.ecp`
//! for the tiny repo its README describes, byte for byte. None of it is
//! written by this code. Read the README first: it lists what the sample
//! cannot show.
//!
//! Every test has two arms, picked by comparing today's identifier with the
//! one the writer used (`WRITER_*`):
//!
//! - **Same identifier:** today's reader must give the consumer what the
//!   writer meant. The test asserts names, kinds, files, lines and call
//!   edges, not that nothing crashed.
//! - **Different identifier:** today's reader must refuse the old data and
//!   the caller must take the rebuild path. The test asserts the refusal.
//!
//! A slice that changes a stored format flips its identifier, so the test of
//! that format moves from the first arm to the second without an edit. That
//! is the intended signal: the migration then has data the old binary wrote
//! to prove it against. A slice that changes what the bytes mean WITHOUT
//! bumping the identifier turns the first arm red.

use ecp_cli::engine::{header_compatible, Engine};
use ecp_cli::parse_cache::ParseCache;
use ecp_cli::session::overlay_reader::{accepted_fragments, decode_view_inputs};
use ecp_cli::session::overlay_writer::write_dirty_events;
use ecp_cli::session::state::classify;
use ecp_cli::session::view_cache;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::dirty_event::PathClass;
use ecp_core::graph::{NodeKind, RelType, GRAPH_FORMAT_VERSION};
use ecp_core::registry::{CommitBuildMeta, BUILDER_FINGERPRINT};
use ecp_core::session::layout;
use ecp_core::session::overlay::SymbolKind;
use ecp_core::session::{
    DirtyEvents, DirtyFiles, OverlayFileInput, SessionMeta, SessionState, FRAGMENT_FORMAT_V5,
    OVERLAY_EDGE_REASON, VIEW_CACHE_FORMAT,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use tempfile::TempDir;

const WRITER_GRAPH_FORMAT: u32 = 15;
const WRITER_FINGERPRINT: &str = "v0.16.4+schema20";
const WRITER_FRAGMENT_FORMAT: u32 = 5;
const WRITER_VIEW_CACHE_FORMAT: u32 = 3;
/// The session-relative dirty manifest the writer kept.
const WRITER_DIRTY_MANIFEST: &str = "dirty_files.json";
const WRITER_PARSE_DIR: &str = "568515f1";
const WRITER_SHA: &str = "e7ca0e6a663e619e7b245890db6cb47653fed6dc";
const COMMIT_DIR: &str = "branch_main__e7ca0e6a663e619e7b245890db6cb47653fed6dc";
const SESSION: &str = "fxsession0001";
const BASE_NODES: usize = 14;
const DIRTY_FILES: [&str; 2] = ["lib.rs", "pkg/core.py"];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stored-by-0.16.4")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A private copy of the fixture's `~/.ecp/<repo>` dir: the readers under test
/// write into their dirs (`.last-used`, a rebuilt `view_cache.bin`), and the
/// checked-in bytes must stay the writer's.
fn copy_repo() -> TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&fixture().join("ecp-home/repo"), &tmp.path().join("repo"));
    tmp
}

fn session_dir(tmp: &TempDir) -> PathBuf {
    tmp.path().join("repo/sessions").join(SESSION)
}

fn graph_in(tmp: &TempDir) -> PathBuf {
    tmp.path()
        .join("repo/commits")
        .join(COMMIT_DIR)
        .join("graph.bin")
}

fn committed(rel: &str) -> Vec<u8> {
    fs::read(fixture().join("source/committed").join(rel)).unwrap()
}

fn set_mtime(path: &Path, mtime_ns: u64) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_nanos(mtime_ns))
        .unwrap();
}

/// The working tree the session was taken from: the committed files, then the
/// two uncommitted ones, each stamped with the mtime the manifest recorded.
fn worktree(tmp: &TempDir) -> PathBuf {
    let root = tmp.path().join("worktree");
    copy_tree(&fixture().join("source/committed"), &root);
    copy_tree(&fixture().join("source/uncommitted"), &root);
    let manifest_path = layout::legacy_dirty_files_path(&session_dir(tmp));
    let manifest = DirtyFiles::read(&manifest_path).unwrap();
    let mut text = fs::read_to_string(&manifest_path).unwrap();
    for (rel, entry) in &manifest.entries {
        let path = root.join(rel);
        set_mtime(&path, entry.mtime_ns);
        let held = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        // The writer stat'ed its own filesystem; NTFS keeps 100 ns, so the
        // recorded stamp cannot be set here. Restate only that number in the
        // copy, as this filesystem holds it; every other byte stays as written.
        if held != entry.mtime_ns {
            text = text.replace(
                &format!("\"mtime_ns\": {}", entry.mtime_ns),
                &format!("\"mtime_ns\": {held}"),
            );
        }
    }
    fs::write(&manifest_path, text).unwrap();
    root
}

fn writer_graph_is_current() -> bool {
    GRAPH_FORMAT_VERSION == WRITER_GRAPH_FORMAT
}

/// Today's reader opens the manifest the writer kept. Since the C3
/// tombstone slice it opens `l1/dirty_events.json`, never the writer's
/// `dirty_files.json`.
fn writer_manifest_is_current() -> bool {
    layout::dirty_events_path(Path::new("")) == Path::new(WRITER_DIRTY_MANIFEST)
}

fn writer_fragments_are_current() -> bool {
    BUILDER_FINGERPRINT == WRITER_FINGERPRINT
        && FRAGMENT_FORMAT_V5 == WRITER_FRAGMENT_FORMAT
        && writer_manifest_is_current()
}

/// `(name, kind, file, first line, last line)`, lines 1-based as a reader of
/// the source file counts them.
type Row = (String, String, String, u32, u32);

fn graph_rows(engine: &Engine) -> Vec<Row> {
    let graph = engine.graph().unwrap();
    let pool = graph.string_pool.as_slice();
    let mut rows: Vec<Row> = graph
        .nodes
        .iter()
        .map(|n| {
            let file = match n.file_idx.to_native() {
                u32::MAX => "<none>".to_string(),
                idx => graph.files[idx as usize].path.resolve(pool).to_string(),
            };
            (
                n.name.resolve(pool).to_string(),
                format!("{:?}", NodeKind::from(&n.kind)),
                file,
                n.span.0.to_native() + 1,
                n.span.2.to_native() + 1,
            )
        })
        .collect();
    rows.sort();
    rows
}

fn row(name: &str, kind: &str, file: &str, first: u32, last: u32) -> Row {
    (name.into(), kind.into(), file.into(), first, last)
}

// ---------------------------------------------------------------- graph.bin

/// Stands for: a `graph.bin` the previous release wrote either answers the
/// same symbols and call edges it answered then (identifier unchanged) or is
/// refused so the caller rebuilds (identifier changed). Never a third thing:
/// a graph read under a layout it was not written for.
#[test]
fn test_graph_bin_written_by_0_16_4_returns_symbols_and_calls_or_is_refused() {
    let dir = fixture().join("ecp-home/repo/commits").join(COMMIT_DIR);
    assert_eq!(
        fs::read_to_string(dir.join("graph.bin.compatible_version"))
            .unwrap()
            .trim(),
        WRITER_GRAPH_FORMAT.to_string(),
        "fixture sidecar disagrees with the writer's recorded format"
    );
    assert_eq!(
        fs::read_to_string(dir.join("graph.bin.builder_fingerprint"))
            .unwrap()
            .trim(),
        WRITER_FINGERPRINT
    );
    let tmp = copy_repo();
    let graph_path = graph_in(&tmp);

    if !writer_graph_is_current() {
        assert!(
            !header_compatible(&graph_path),
            "graph.bin format {GRAPH_FORMAT_VERSION} accepted a format-{WRITER_GRAPH_FORMAT} graph"
        );
        let err = Engine::load(&graph_path)
            .err()
            .expect("an old-format graph must not load");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        return;
    }

    assert!(header_compatible(&graph_path));
    let engine = Engine::load(&graph_path).expect("the previous release's graph must load");
    let rows = graph_rows(&engine);
    assert_eq!(rows.len(), BASE_NODES, "{rows:#?}");

    let functions: Vec<Row> = rows.iter().filter(|r| r.1 == "Function").cloned().collect();
    assert_eq!(
        functions,
        vec![
            row("compute", "Function", "pkg/core.py", 1, 2),
            row("go", "Function", "app.py", 4, 5),
            row("helper", "Function", "app.py", 8, 9),
            row("helper", "Function", "lib.rs", 1, 3),
            row("helper", "Function", "pkg/core.py", 5, 6),
            row("helper", "Function", "web/util.ts", 1, 3),
            row("render", "Function", "web/util.ts", 5, 7),
            row("run", "Function", "lib.rs", 5, 7),
        ]
    );
    // The line a consumer is sent to is the line that declares the symbol.
    for (name, _, file, first, _) in &functions {
        let source = String::from_utf8(committed(file)).unwrap();
        let declaration = source.lines().nth(*first as usize - 1).unwrap();
        assert!(
            declaration.contains(name.as_str()),
            "{file}:{first} is `{declaration}`, not the declaration of {name}"
        );
    }
    let others: Vec<(String, String)> = rows
        .iter()
        .filter(|r| r.1 != "Function")
        .map(|r| (r.1.clone(), r.0.clone()))
        .collect();
    assert_eq!(
        others,
        vec![
            ("Process".to_string(), "Go \u{2192} Helper".to_string()),
            ("File".to_string(), "__init__.py".to_string()),
            ("File".to_string(), "app.py".to_string()),
            ("File".to_string(), "core.py".to_string()),
            ("File".to_string(), "lib.rs".to_string()),
            ("File".to_string(), "util.ts".to_string()),
        ]
    );

    // Four symbols called `helper`, one per file: present, distinct, and each
    // call binds to the one in its own file (not to a same-name neighbour).
    let graph = engine.graph().unwrap();
    let pool = graph.string_pool.as_slice();
    let mut helper_uids: Vec<u64> = graph
        .nodes
        .iter()
        .filter(|n| {
            n.name.resolve(pool) == "helper" && NodeKind::from(&n.kind) == NodeKind::Function
        })
        .map(|n| n.uid.to_native())
        .collect();
    helper_uids.sort_unstable();
    helper_uids.dedup();
    assert_eq!(helper_uids.len(), 4, "same-name symbols collapsed");

    let file_of = |idx: u32| -> String {
        let n = &graph.nodes[idx as usize];
        graph.files[n.file_idx.to_native() as usize]
            .path
            .resolve(pool)
            .to_string()
    };
    let mut calls: Vec<(String, String, String, String)> = graph
        .edges
        .iter()
        .filter(|e| RelType::from(&e.rel_type) == RelType::Calls)
        .map(|e| {
            let (s, t) = (e.source.to_native(), e.target.to_native());
            (
                graph.nodes[s as usize].name.resolve(pool).to_string(),
                file_of(s),
                graph.nodes[t as usize].name.resolve(pool).to_string(),
                file_of(t),
            )
        })
        .collect();
    calls.sort();
    let call = |a: &str, af: &str, b: &str, bf: &str| (a.into(), af.into(), b.into(), bf.into());
    assert_eq!(
        calls,
        vec![
            call("compute", "pkg/core.py", "helper", "pkg/core.py"),
            call("go", "app.py", "compute", "pkg/core.py"),
            call("render", "web/util.ts", "helper", "web/util.ts"),
            call("run", "lib.rs", "helper", "lib.rs"),
        ]
    );
}

/// Stands for: the per-commit `meta.json` the freshness check reads still
/// parses and names the writer, so an upgrade can tell whose graph it holds.
#[test]
fn test_commit_meta_written_by_0_16_4_returns_writer_identity() {
    let tmp = copy_repo();
    let meta = CommitBuildMeta::read(
        &tmp.path()
            .join("repo/commits")
            .join(COMMIT_DIR)
            .join("meta.json"),
    )
    .expect("meta.json written by 0.16.4 must parse");
    assert_eq!(meta.sha, WRITER_SHA);
    assert_eq!(meta.source_id.as_deref(), Some("main"));
    assert_eq!(meta.node_count as usize, BASE_NODES);
    assert_eq!(
        meta.builder_fingerprint.as_deref(),
        Some(WRITER_FINGERPRINT)
    );
    assert_eq!(meta.binary_commit_sha.as_deref(), Some("a9b0c9c"));
}

// -------------------------------------------------------------- parse cache

/// `(name, first line, last line, calls)` of the functions of a parsed file.
type ParsedFn = (String, u32, u32, Vec<String>);

fn parsed_functions(graph: &LocalGraph) -> Vec<ParsedFn> {
    let mut out: Vec<ParsedFn> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .map(|n| (n.name.clone(), n.span.0 + 1, n.span.2 + 1, n.calls.clone()))
        .collect();
    out.sort_by_key(|f| f.1);
    out
}

fn parsed_fn(name: &str, first: u32, last: u32, calls: &[&str]) -> ParsedFn {
    (
        name.into(),
        first,
        last,
        calls.iter().map(|c| c.to_string()).collect(),
    )
}

/// Stands for: the parser output the previous release cached is replayed to
/// today's builder only when the same parser rules wrote it
/// (`BUILDER_FINGERPRINT` names the directory). Otherwise the lookup misses,
/// the file is parsed again, and the old directory is left to gc.
#[test]
fn test_parse_cache_written_by_0_16_4_replays_parse_or_misses() {
    let tmp = copy_repo();
    let repo = tmp.path().join("repo");
    let cache = ParseCache::open(&repo).unwrap();
    let files = [
        "app.py",
        "pkg/__init__.py",
        "pkg/core.py",
        "web/util.ts",
        "lib.rs",
    ];
    for rel in files {
        let hash = xxhash_rust::xxh3::xxh3_64(&committed(rel)).to_le_bytes();
        let name = cache
            .path_for(Path::new(rel), &hash)
            .file_name()
            .unwrap()
            .to_owned();
        assert!(
            repo.join("parse_cache")
                .join(WRITER_PARSE_DIR)
                .join(&name)
                .exists(),
            "fixture holds no entry for {rel}: key scheme or fixture changed"
        );
        let got = cache.get(Path::new(rel), &hash);
        if ParseCache::current_dir_name() != WRITER_PARSE_DIR {
            assert!(got.is_none(), "{rel}: another parser's output was replayed");
            continue;
        }
        let graph = got.unwrap_or_else(|| panic!("{rel}: the writer's own entry did not decode"));
        assert_eq!(graph.file_path, Path::new(rel));
        assert_eq!(graph.content_hash, hash);
        let expected: Vec<ParsedFn> = match rel {
            "app.py" => vec![
                parsed_fn("go", 4, 5, &["compute"]),
                parsed_fn("helper", 8, 9, &[]),
            ],
            "lib.rs" => vec![
                parsed_fn("helper", 1, 3, &[]),
                parsed_fn("run", 5, 7, &["helper"]),
            ],
            "pkg/core.py" => vec![
                parsed_fn("compute", 1, 2, &["helper"]),
                parsed_fn("helper", 5, 6, &[]),
            ],
            "web/util.ts" => vec![
                parsed_fn("helper", 1, 3, &[]),
                parsed_fn("render", 5, 7, &["helper"]),
            ],
            _ => vec![],
        };
        assert_eq!(parsed_functions(&graph), expected, "{rel}");
        if rel == "app.py" {
            assert!(
                graph.imports.iter().any(|i| i.imported_name == "compute"),
                "app.py's `from pkg.core import compute` lost: {:?}",
                graph.imports
            );
        }
    }
}

// ------------------------------------------------- dirty manifest and meta

/// Stands for: the session's dirty manifest names the files the user edited
/// and the symbols in them. Today no query reads it (see
/// `writer_manifest_is_current`), but `admin gc` still reads its fragment
/// names to sweep the legacy namespace, so it must keep parsing as written.
#[test]
fn test_dirty_manifest_written_by_0_16_4_returns_dirty_paths_and_symbols() {
    let tmp = copy_repo();
    let manifest = DirtyFiles::read(&layout::legacy_dirty_files_path(&session_dir(&tmp)))
        .expect("dirty_files.json written by 0.16.4 must parse");
    assert_eq!(manifest.version, 1);
    assert_eq!(
        manifest.builder_fingerprint.as_deref(),
        Some(WRITER_FINGERPRINT)
    );
    assert_eq!(
        manifest.from_current_builder(),
        BUILDER_FINGERPRINT == WRITER_FINGERPRINT
    );
    assert_eq!(
        manifest
            .entries
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        DIRTY_FILES
    );
    for (rel, entry) in &manifest.entries {
        assert_eq!(entry.format, WRITER_FRAGMENT_FORMAT, "{rel}");
        assert!(!entry.parse_failed, "{rel}");
        assert_eq!(entry.fragment_id, entry.content_hash, "{rel}");
        assert!(entry.tantivy_delta_segment.is_none(), "{rel}");
        assert!(entry.mtime_ns > 0, "{rel}");
    }
    let symbols = |rel: &str| -> Vec<(String, SymbolKind, u32, u32)> {
        manifest.entries[rel]
            .dirty_symbols
            .iter()
            .map(|s| {
                assert_eq!(s.file, rel);
                (s.name.clone(), s.kind, s.line_start, s.line_end)
            })
            .collect()
    };
    assert_eq!(
        symbols("lib.rs"),
        vec![
            ("helper".to_string(), SymbolKind::Function, 1, 3),
            ("run".to_string(), SymbolKind::Function, 5, 7),
        ]
    );
    assert_eq!(
        symbols("pkg/core.py"),
        vec![
            ("compute".to_string(), SymbolKind::Function, 1, 2),
            ("helper".to_string(), SymbolKind::Function, 5, 6),
            ("fresh_only".to_string(), SymbolKind::Function, 9, 10),
        ]
    );
}

/// Stands for: a session the previous release left behind is still found and
/// tied to its base commit. While today's reader opens the writer's manifest
/// it holds two dirty files, so `ecp` keeps serving the overlay; once the
/// manifest moved, it holds none (the files re-parse into the new one), and
/// is never called stale.
#[test]
fn test_session_written_by_0_16_4_classifies_as_augmented_with_two_fragments() {
    let tmp = copy_repo();
    let meta = SessionMeta::read(&session_dir(&tmp).join("session_meta.json"))
        .expect("session_meta.json written by 0.16.4 must parse");
    assert_eq!(meta.session_id, SESSION);
    assert_eq!(meta.base_sha, WRITER_SHA);
    assert_eq!(meta.overlay_version, 2);
    assert_eq!(meta.agent_name, None);
    assert_eq!(meta.watcher_pid, None);
    assert_eq!(meta.last_drained_offset, 0);

    let state = classify(&tmp.path().join("repo"), SESSION);
    if !writer_manifest_is_current() {
        assert_eq!(
            state,
            SessionState::PureReference {
                base_sha: WRITER_SHA.to_string(),
                l2_dirname: COMMIT_DIR.to_string(),
            },
            "the legacy manifest was read as today's dirty set"
        );
        return;
    }
    match state {
        SessionState::AugmentedReference {
            base_sha,
            l2_dirname,
            fragment_count,
        } => {
            assert_eq!(base_sha, WRITER_SHA);
            assert_eq!(l2_dirname, COMMIT_DIR);
            assert_eq!(fragment_count, 2);
        }
        other => panic!("a live session was classified {other:?}"),
    }
}

// ------------------------------------------------------------ L1 fragments

/// `(name, first line, last line, calls)` of an overlay file input.
fn input_symbols(input: &OverlayFileInput) -> Vec<(String, u32, u32, Vec<String>)> {
    let mut out: Vec<_> = input
        .symbols
        .iter()
        .map(|s| (s.name.clone(), s.start_line, s.end_line, s.calls.clone()))
        .collect();
    out.sort_by_key(|s| s.1);
    out
}

/// Stands for: the fragment encoding the previous release wrote (V5) either
/// yields the dirty files' symbols, lines and calls to the overlay (same
/// encoding and parser rules) or is skipped whole so the next refresh
/// re-parses (anything else). A skipped fragment must never half-decode.
#[test]
fn test_fragments_written_by_0_16_4_return_symbols_and_calls_or_are_skipped() {
    let tmp = copy_repo();
    let (accepted, _) = accepted_fragments(&session_dir(&tmp), &worktree(&tmp)).unwrap();
    let inputs = decode_view_inputs(&accepted);

    if !writer_fragments_are_current() {
        assert!(
            accepted.is_empty(),
            "fragments of another builder or encoding were served"
        );
        assert!(inputs.is_empty());
        return;
    }

    let by_path = |rel: &str| {
        inputs
            .iter()
            .find(|i| i.rel_path == rel)
            .unwrap_or_else(|| panic!("no overlay input for dirty file {rel}"))
    };
    assert_eq!(inputs.len(), 2);
    let call = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    let sym =
        |name: &str, first, last, calls: &[&str]| (name.to_string(), first, last, call(calls));
    assert_eq!(
        input_symbols(by_path("lib.rs")),
        vec![sym("helper", 1, 3, &[]), sym("run", 5, 7, &["helper"])]
    );
    assert_eq!(
        input_symbols(by_path("pkg/core.py")),
        vec![
            sym("compute", 1, 2, &["helper"]),
            sym("helper", 5, 6, &[]),
            sym("fresh_only", 9, 10, &["compute"]),
        ]
    );
    for input in &inputs {
        assert!(
            input.symbols.iter().all(|s| s.kind == NodeKind::Function),
            "{}: kinds changed",
            input.rel_path
        );
    }
}

/// Stands for: an entry whose file changed after the fragment was written is
/// not served (stale overlay data must not beat fresh base data). The old
/// manifest's mtime is the only thing that tells.
#[test]
fn test_fragments_written_by_0_16_4_skip_the_file_edited_since() {
    if !writer_fragments_are_current() {
        return; // nothing is served at all; the test above covers that
    }
    let tmp = copy_repo();
    let root = worktree(&tmp);
    let manifest = DirtyFiles::read(&layout::legacy_dirty_files_path(&session_dir(&tmp))).unwrap();
    set_mtime(
        &root.join("pkg/core.py"),
        manifest.entries["pkg/core.py"].mtime_ns + 1_000_000_000,
    );
    let (accepted, _) = accepted_fragments(&session_dir(&tmp), &root).unwrap();
    let inputs = decode_view_inputs(&accepted);
    let paths: Vec<&str> = inputs.iter().map(|i| i.rel_path.as_str()).collect();
    assert_eq!(paths, vec!["lib.rs"]);
}

/// Stands for: today's binary keeps its tombstones in a namespace the older
/// binary of the same fingerprint never opens. The older writer reads its
/// whole `dirty_files.json` into a struct without `change` / `class` and
/// writes it back; had the tombstone lived there, it would come back as a
/// `modified` entry. Here the older writer rewrites its manifest and bins
/// after today's binary recorded `lib.rs` as deleted: the tombstone keeps
/// its bytes, and no entry of the old manifest reaches the overlay, though
/// its `pkg/core.py` entry still matches the file on disk.
#[test]
fn test_dirty_events_old_writer_manifest_beside_new_namespace_keeps_tombstone() {
    let tmp = copy_repo();
    let session = session_dir(&tmp);
    let root = worktree(&tmp);
    fs::remove_file(root.join("lib.rs")).unwrap();
    let cap = ecp_core::config::resolve_size_cap(&root);
    write_dirty_events(
        &session,
        cap,
        &[],
        &[],
        &HashMap::new(),
        &[("lib.rs".to_string(), PathClass::SOURCE)],
    )
    .unwrap();
    let new_manifest = layout::dirty_events_path(&session);
    let written = fs::read(&new_manifest).unwrap();

    // The older writer's save: its whole manifest round-trips through its
    // own entry type, and its bins are written again.
    let legacy_path = layout::legacy_dirty_files_path(&session);
    let legacy = DirtyFiles::read(&legacy_path).unwrap();
    DirtyFiles::write_atomic(&legacy_path, &legacy).unwrap();
    for entry in legacy.entries.values() {
        let bin = layout::legacy_overlay_dir(&session).join(format!("{}.bin", entry.fragment_id));
        let bytes = fs::read(&bin).unwrap();
        fs::write(&bin, bytes).unwrap();
    }
    // Control: an entry of the old manifest is still servable by mtime.
    let core_mtime = fs::metadata(root.join("pkg/core.py"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    assert_eq!(legacy.entries["pkg/core.py"].mtime_ns, core_mtime);

    let (accepted, _) = accepted_fragments(&session, &root).unwrap();
    let state = classify(&tmp.path().join("repo"), SESSION);

    assert!(
        accepted.is_empty(),
        "an entry of the older writer's manifest was served"
    );
    if writer_graph_is_current() {
        let engine = Engine::load(graph_in(&tmp))
            .unwrap()
            .with_overlay(session.clone(), root.clone());
        assert!(
            engine.overlay_view().is_none(),
            "the overlay served a fragment"
        );
    }
    assert!(
        matches!(
            state,
            SessionState::AugmentedReference {
                fragment_count: 1,
                ..
            }
        ),
        "{state:?}"
    );
    assert_eq!(
        fs::read(&new_manifest).unwrap(),
        written,
        "the tombstone was rewritten"
    );
    let manifest = DirtyEvents::read(&new_manifest).unwrap();
    assert!(manifest.is_current(cap));
    assert_eq!(manifest.entries.keys().collect::<Vec<_>>(), ["lib.rs"]);
    let tombstone = &manifest.entries["lib.rs"];
    assert!(tombstone.is_tombstone());
    assert_eq!(tombstone.class, PathClass::SOURCE);
}

// --------------------------------------------------------------- view cache

/// Stands for: the view cache never serves one binary's view to another (the
/// key covers the running executable), and when graph, fragments and layout
/// are all the writer's, the cached body equals what today's code builds from
/// the same inputs. The second half is the rule that any change to what
/// `OverlayView::build` produces must bump `VIEW_CACHE_FORMAT`.
#[test]
fn test_view_cache_written_by_0_16_4_misses_under_todays_key_and_matches_a_rebuild() {
    let tmp = copy_repo();
    let session = session_dir(&tmp);
    let graph_path = graph_in(&tmp);
    let bytes = fs::read(session.join("view_cache.bin")).unwrap();
    let stored_key = u64::from_le_bytes(bytes[..8].try_into().unwrap());

    let meta = fs::metadata(&graph_path).unwrap();
    let todays = view_cache::key(
        (graph_path.as_path(), meta.len(), meta.modified().unwrap()),
        BASE_NODES,
        0,
    )
    .expect("key");
    assert_ne!(
        todays, stored_key,
        "a view cache key ignores the running binary"
    );
    assert!(view_cache::read(&session, todays, BASE_NODES).is_none());

    let same_everything = writer_graph_is_current()
        && writer_fragments_are_current()
        && VIEW_CACHE_FORMAT == WRITER_VIEW_CACHE_FORMAT;
    if !same_everything {
        return; // the miss above is the whole contract: the view is rebuilt
    }

    let cached = view_cache::read(&session, stored_key, BASE_NODES)
        .expect("the previous release's view cache must decode under its own key");

    let mut nodes: Vec<(String, String, u32, u32)> = cached
        .virtual_nodes()
        .iter()
        .map(|n| {
            (
                n.rel_path.to_string(),
                n.name.clone(),
                n.start_line,
                n.end_line,
            )
        })
        .collect();
    nodes.sort();
    let vnode = |f: &str, n: &str, a, b| (f.to_string(), n.to_string(), a, b);
    assert_eq!(
        nodes,
        vec![
            vnode("lib.rs", "helper", 1, 3),
            vnode("lib.rs", "run", 5, 7),
            vnode("pkg/core.py", "compute", 1, 2),
            vnode("pkg/core.py", "fresh_only", 9, 10),
            vnode("pkg/core.py", "helper", 5, 6),
        ]
    );
    let end = |idx: u32| {
        let n = cached
            .node(idx)
            .expect("an overlay edge ends in a base node");
        (n.name.clone(), n.rel_path.to_string())
    };
    let mut calls: Vec<((String, String), (String, String))> = cached
        .edges()
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .map(|e| {
            assert_eq!(e.reason, OVERLAY_EDGE_REASON, "edge reason remapped");
            (end(e.source), end(e.target))
        })
        .collect();
    calls.sort();
    let at = |n: &str, f: &str| (n.to_string(), f.to_string());
    assert_eq!(
        calls,
        vec![
            (at("compute", "pkg/core.py"), at("helper", "pkg/core.py")),
            (
                at("fresh_only", "pkg/core.py"),
                at("compute", "pkg/core.py")
            ),
            (at("run", "lib.rs"), at("helper", "lib.rs")),
        ]
    );

    let engine = Engine::load(&graph_path)
        .unwrap()
        .with_overlay(session.clone(), worktree(&tmp));
    let built = engine.overlay_view().expect("today's code builds a view");
    assert_eq!(
        built, &cached,
        "the same graph and fragments now build a different view: bump VIEW_CACHE_FORMAT"
    );

    // The clean caller `go` still reaches the edited `compute`: the base
    // node redirects to its overlay successor.
    let graph = engine.graph().unwrap();
    let pool = graph.string_pool.as_slice();
    let base_compute = graph
        .nodes
        .iter()
        .position(|n| {
            n.name.resolve(pool) == "compute"
                && graph.files[n.file_idx.to_native() as usize]
                    .path
                    .resolve(pool)
                    == "pkg/core.py"
        })
        .expect("base compute") as u32;
    let successor = cached
        .redirect(base_compute)
        .expect("compute was edited, not deleted");
    assert_eq!(end(successor), at("compute", "pkg/core.py"));
}
