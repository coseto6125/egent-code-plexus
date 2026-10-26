//! One cache that holds data of two releases, read by today's `ecp`.
//!
//! `stored_formats_prev_release.rs` reads each stored format the previous
//! release (0.16.4) wrote on its own, through library calls. A real cache
//! mixes them: a `graph.bin` from today's index beside a parse cache and a
//! session from 0.16.4, or the other way round. These tests build such a cache
//! and run the real `ecp` binary against it. Each asserts what a query
//! returns (symbol names, kinds, files, lines, call edges), never only that
//! the read succeeded.
//!
//! Where the 0.16.4 pieces come from: `fixtures/stored-by-0.16.4/`. Today's
//! pieces come from `ecp admin index` run on a copy of the same sources, in a
//! scratch `ECP_HOME`. A mixed cache takes some pieces from each.
//!
//! What the fixture cannot show (its README lists it; a green test here says
//! nothing about these):
//!
//! - Languages: only Python, TypeScript and Rust. No class, no method, no
//!   heritage, no enum variant, no field read, no import alias.
//! - One fragment format (V5) and one builder: no V2-V4 bin, no entry with
//!   `parse_failed`, no fragment file missing.
//! - The dirty manifest holds no deleted or renamed file.
//! - One session, one commit, no `.gen` generation, no stray tmp file, no lock
//!   contention in the fixture itself.
//! - The view cache key hashes the running executable and the graph file, so
//!   a cache HIT cannot be built from the fixture. Only the miss is tested.
//!
//! Bytes the tests restate: a fresh git repo has another commit SHA than the
//! repo the writer used, and the writer's scratch paths no longer exist. The
//! tests rewrite the identity fields only: the commit dir name,
//! `graph.bin.head_sha`, `meta.json` `sha` and `built_from_worktree`, and
//! `session_meta.json` `base_sha` and `source_worktree`. `graph.bin`, the
//! parse cache entries, `dirty_files.json`, `graph_overlay/*.bin` and
//! `view_cache.bin` keep the writer's bytes.
//!
//! Identifiers the tests depend on:
//!
//! | Test | Identifier |
//! |---|---|
//! | new graph, old parse cache | `BUILDER_FINGERPRINT` (names the parse cache dir) |
//! | old graph, new parse cache | `GRAPH_FORMAT_VERSION`, `BUILDER_FINGERPRINT` |
//! | new graph, old fragments | `FRAGMENT_FORMAT_V7`, `DIRTY_EVENTS_FORMAT` |
//! | old manifest, new dirty events | `DIRTY_EVENTS_FORMAT` (the `l1/` namespace) |
//! | old view cache, new graph | `VIEW_CACHE_FORMAT` (and the key's binary hash) |
//! | truncated / empty pieces / two sessions / concurrent | `GRAPH_FORMAT_VERSION`, `FRAGMENT_FORMAT_V7` |
//!
//! Edge states that do not apply: these tests take no free-text input, so the
//! list-for-scalar and whitespace-only states have no input to feed. The
//! other states are tests: truncated `graph.bin`, empty piece directories,
//! two sessions in one cache, a concurrent duplicate call, and machine-parsed
//! output (every answer is read from `--format json`).

mod common;

use common::stored::{
    committed, copy_tree, fixture, set_mtime, COMMIT_DIR, SESSION, WRITER_FINGERPRINT,
    WRITER_GRAPH_FORMAT, WRITER_PARSE_DIR,
};
use common::{commit_all, ecp_bin, run_git};
use ecp_cli::parse_cache::ParseCache;
use ecp_cli::repo_identity::repo_dir_name_for_cwd;
use ecp_core::graph::GRAPH_FORMAT_VERSION;
use ecp_core::registry::BUILDER_FINGERPRINT;
use ecp_core::session::overlay::DIRTY_EVENTS_FORMAT;
use ecp_core::session::{layout, DirtyEvents, DirtyFiles, FRAGMENT_FORMAT_V7};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// Session ids of today's binary in these tests (the fixture's is `SESSION`).
const TODAY_SESSION: &str = "mixsession0001";
const OTHER_SESSION: &str = "mixsession0002";
const COMMITTED_FILES: [&str; 5] = [
    "app.py",
    "pkg/__init__.py",
    "pkg/core.py",
    "web/util.ts",
    "lib.rs",
];
/// Host variables that would send a query through another session or cap the
/// file size.
const HOST_VARS: [&str; 6] = [
    "ECP_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
    "GEMINI_CLI_SESSION_ID",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "ECP_MAX_FILE_BYTES",
];

/// `(name, kind, file, 1-based line)` of a `find` match.
type Hit = (String, String, String, u64);

fn fn_hit(name: &str, file: &str, line: u64) -> Hit {
    (
        name.to_string(),
        "Function".to_string(),
        file.to_string(),
        line,
    )
}

/// The four same-name functions of the fixture, one per file.
fn helper_hits() -> Vec<Hit> {
    vec![
        fn_hit("helper", "app.py", 8),
        fn_hit("helper", "lib.rs", 1),
        fn_hit("helper", "pkg/core.py", 5),
        fn_hit("helper", "web/util.ts", 1),
    ]
}

/// Stands for: the symbol the uncommitted `pkg/core.py` declares at line 9.
fn fresh_only_hits() -> Vec<Hit> {
    vec![fn_hit("fresh_only", "pkg/core.py", 9)]
}

/// Stands for: `fresh_only` calls `compute`, both in `pkg/core.py`.
fn fresh_only_calls() -> Vec<String> {
    vec![json!(["pkg/core.py", "compute", "pkg/core.py"]).to_string()]
}

fn json_of(out: &Output) -> Value {
    let text = String::from_utf8_lossy(&out.stdout);
    let start = text
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout: {text}"));
    serde_json::from_str(&text[start..]).unwrap_or_else(|e| panic!("non-JSON ({e}): {text}"))
}

/// The `find` matches whose name is exactly `name`, sorted.
fn hits(out: &Output, name: &str) -> Vec<Hit> {
    let v = json_of(out);
    let none = Vec::new();
    let mut hits: Vec<Hit> = v["matches"]
        .as_array()
        .unwrap_or(&none)
        .iter()
        .filter(|m| m["name"] == name)
        .map(|m| {
            (
                name.to_string(),
                m["kind"].as_str().unwrap_or("").to_string(),
                m["file"].as_str().unwrap_or("").to_string(),
                m["line"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    hits.sort();
    hits
}

/// A full rebuild announces itself on stderr (`l2.rebuilt` after a refused
/// graph, `l2.built` when none existed).
fn rebuilt(out: &Output) -> bool {
    let err = String::from_utf8_lossy(&out.stderr);
    err.contains("l2.rebuilt") || err.contains("l2.built")
}

/// Every file of the legacy namespace of one session, by path, with its bytes.
fn legacy_snapshot(session: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = vec![layout::legacy_dirty_files_path(session)];
    let mut bins: Vec<PathBuf> = fs::read_dir(layout::legacy_overlay_dir(session))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    bins.sort();
    files.extend(bins);
    files
        .into_iter()
        .map(|p| {
            let bytes = fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect()
}

fn l1_manifest(session: &Path) -> DirtyEvents {
    DirtyEvents::read(&layout::dirty_events_path(session))
        .expect("today's binary writes l1/dirty_events.json")
}

/// Cut every `graph.bin` under `<repo>/commits/*/` in half.
fn truncate_graphs(repo: &Path) {
    let mut cut = 0;
    for entry in fs::read_dir(repo.join("commits")).unwrap().flatten() {
        let graph = entry.path().join("graph.bin");
        if graph.is_file() {
            let bytes = fs::read(&graph).unwrap();
            fs::write(&graph, &bytes[..bytes.len() / 2]).unwrap();
            cut += 1;
        }
    }
    assert!(cut > 0, "setup: no graph.bin to truncate under {repo:?}");
}

/// The fixture's sources as a git repo, and the scratch homes around it.
struct Rig {
    tmp: TempDir,
    wt: PathBuf,
    user_home: PathBuf,
    sha: String,
    repo_dir: String,
}

impl Rig {
    fn new() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        copy_tree(&fixture().join("source/committed"), &wt);
        run_git(&wt, &["init", "-q", "-b", "main"]);
        commit_all(&wt, "init");
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&wt)
            .output()
            .unwrap();
        assert!(head.status.success());
        let sha = String::from_utf8(head.stdout).unwrap().trim().to_string();
        let user_home = tmp.path().join("user-home");
        fs::create_dir_all(&user_home).unwrap();
        let repo_dir = repo_dir_name_for_cwd(&wt).unwrap();
        Rig {
            tmp,
            wt,
            user_home,
            sha,
            repo_dir,
        }
    }

    /// A scratch `ECP_HOME`.
    fn home(&self, name: &str) -> PathBuf {
        let home = self.tmp.path().join(name);
        fs::create_dir_all(&home).unwrap();
        home
    }

    fn repo_root(&self, home: &Path) -> PathBuf {
        home.join(&self.repo_dir)
    }

    /// Run `ecp` in the worktree under `home`; panic when it fails.
    fn ecp(&self, home: &Path, session: Option<&str>, args: &[&str]) -> Output {
        let mut cmd = Command::new(ecp_bin());
        cmd.args(args)
            .current_dir(&self.wt)
            .env("ECP_HOME", home)
            .env("HOME", &self.user_home)
            .env("ECP_SKIP_BG_REBUILD", "1")
            .env("ECP_NO_PROGRESS", "1");
        for key in HOST_VARS {
            cmd.env_remove(key);
        }
        if let Some(id) = session {
            cmd.env("ECP_SESSION_ID", id);
        }
        let out = cmd.output().expect("run ecp");
        assert!(
            out.status.success(),
            "ecp {args:?} failed:\nstderr: {}\nstdout: {}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        out
    }

    /// Today's index of the worktree as it is; returns the repo dir inside
    /// `home`. Call it while the tree is clean.
    fn index(&self, home: &Path, force: bool) -> PathBuf {
        let mut args = vec!["admin", "index", "--repo", "."];
        if force {
            args.push("--force");
        }
        self.ecp(home, None, &args);
        self.repo_root(home)
    }

    /// The two uncommitted files of the fixture, over the committed ones.
    fn edit(&self) {
        for rel in ["lib.rs", "pkg/core.py"] {
            let edited = fs::read(fixture().join("source/uncommitted").join(rel)).unwrap();
            fs::write(self.wt.join(rel), edited).unwrap();
        }
    }

    fn find_all(&self, home: &Path, session: Option<&str>, name: &str) -> Vec<Hit> {
        let out = self.ecp(home, session, &["find", name, "--all", "--format", "json"]);
        hits(&out, name)
    }

    /// Cypher rows as compact JSON strings, sorted.
    fn rows(&self, home: &Path, session: Option<&str>, query: &str) -> Vec<String> {
        let out = self.ecp(home, session, &["cypher", query, "--format", "json"]);
        let v = json_of(&out);
        let mut rows: Vec<String> = v["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("no rows in {v}"))
            .iter()
            .map(Value::to_string)
            .collect();
        rows.sort();
        rows
    }

    /// `(file, callee, callee file)` rows of the Calls edges out of `name`,
    /// duplicates removed.
    fn calls_from(&self, home: &Path, session: Option<&str>, name: &str) -> Vec<String> {
        let query = format!(
            "MATCH (a {{name: '{name}'}})-[:Calls]->(b) RETURN a.filePath, b.name, b.filePath"
        );
        let mut rows = self.rows(home, session, &query);
        rows.dedup();
        rows
    }

    /// Every node and every Calls edge of the graph a query reads.
    fn dump(&self, home: &Path, session: Option<&str>) -> (Vec<String>, Vec<String>) {
        let nodes = self.rows(
            home,
            session,
            "MATCH (n) RETURN n.name, n.kind, n.filePath, n.startLine, n.endLine",
        );
        let edges = self.rows(
            home,
            session,
            "MATCH (a)-[r:Calls]->(b) RETURN a.name, a.filePath, a.startLine, b.name, b.filePath, b.startLine, r.reason, r.confidence",
        );
        (nodes, edges)
    }

    /// The answer the dirty-file tests compare: the symbol the uncommitted
    /// `pkg/core.py` adds, and what it calls.
    fn dirty_answer(&self, home: &Path, session: &str) -> (Vec<Hit>, Vec<String>) {
        (
            self.find_all(home, Some(session), "fresh_only"),
            self.calls_from(home, Some(session), "fresh_only"),
        )
    }

    /// 0.16.4's commit dir (`graph.bin` and sidecars), named for this repo's
    /// HEAD. Only the identity fields are restated.
    fn stage_prev_commit(&self, repo: &Path) {
        let dir = repo
            .join("commits")
            .join(format!("branch_main__{}", self.sha));
        copy_tree(
            &fixture().join("ecp-home/repo/commits").join(COMMIT_DIR),
            &dir,
        );
        fs::write(dir.join("graph.bin.head_sha"), format!("{}\n", self.sha)).unwrap();
        let meta_path = dir.join("meta.json");
        let mut meta: Value = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
        meta["sha"] = Value::from(self.sha.clone());
        meta["built_from_worktree"] = Value::from(
            fs::canonicalize(&self.wt)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        fs::write(&meta_path, serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
    }

    /// 0.16.4's parse cache, byte for byte.
    fn stage_prev_parse_cache(&self, repo: &Path) {
        copy_tree(
            &fixture()
                .join("ecp-home/repo/parse_cache")
                .join(WRITER_PARSE_DIR),
            &repo.join("parse_cache").join(WRITER_PARSE_DIR),
        );
    }

    /// 0.16.4's session dir (`dirty_files.json`, `graph_overlay/`, and
    /// `view_cache.bin` when asked). Only the identity fields of
    /// `session_meta.json` are restated. Returns the session dir.
    fn stage_prev_session(&self, repo: &Path, with_view_cache: bool) -> PathBuf {
        let dir = repo.join("sessions").join(SESSION);
        copy_tree(
            &fixture().join("ecp-home/repo/sessions").join(SESSION),
            &dir,
        );
        if !with_view_cache {
            fs::remove_file(dir.join("view_cache.bin")).unwrap();
        }
        let meta_path = dir.join("session_meta.json");
        let mut meta: Value = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
        meta["base_sha"] = Value::from(self.sha.clone());
        meta["source_worktree"] = Value::from(
            fs::canonicalize(&self.wt)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        fs::write(&meta_path, serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
        dir
    }
}

// ------------------------------------------------------------ table tests

/// Pass/fail criterion. A parse cache entry written under another builder
/// fingerprint is replayed into today's build, or leaks into a rebuild: the
/// graph then differs from a fresh index (the bug gives a node or Calls edge
/// missing or changed in the dump). The fix keys the parse cache directory on
/// `BUILDER_FINGERPRINT`: every 0.16.4 entry misses, and the dump of the
/// mixed cache equals the dump of a fresh index, before and after a forced
/// reindex.
///
/// The dump rows stand for the node contract (name, kind, file, lines) and the
/// Calls edge contract (endpoints, reason, confidence) of `ecp cypher`.
///
/// Expected on HEAD: pass. `BUILDER_FINGERPRINT` is `v<version>+schema41`, the
/// writer's was `v0.16.4+schema20`, so the lookups miss.
#[test]
fn test_mixed_cache_new_graph_old_parse_cache_query_matches_fresh() {
    let rig = Rig::new();
    let fresh_home = rig.home("fresh");
    rig.index(&fresh_home, false);
    let fresh = rig.dump(&fresh_home, None);
    assert!(!fresh.0.is_empty(), "setup: the fresh graph has no node");
    assert!(
        fresh
            .1
            .iter()
            .any(|r| r.starts_with("[\"run\",\"lib.rs\",") && r.contains("\"helper\",\"lib.rs\"")),
        "setup: the fresh graph lacks the run -> helper edge: {:?}",
        fresh.1
    );

    let mixed_home = rig.home("mixed");
    let repo = rig.index(&mixed_home, false);
    let _ = fs::remove_dir_all(repo.join("parse_cache"));
    rig.stage_prev_parse_cache(&repo);

    let cache = ParseCache::open(&repo).unwrap();
    let same_dir = ParseCache::current_dir_name() == WRITER_PARSE_DIR;
    for rel in COMMITTED_FILES {
        let hash = xxhash_rust::xxh3::xxh3_64(&committed(rel)).to_le_bytes();
        assert_eq!(
            cache.get(Path::new(rel), &hash).is_some(),
            same_dir,
            "{rel}: the parse cache served (or refused) an entry against its fingerprint"
        );
    }

    assert_eq!(
        rig.dump(&mixed_home, None),
        fresh,
        "the graph differs from a fresh index with 0.16.4's parse cache beside it"
    );
    rig.index(&mixed_home, true);
    assert_eq!(
        rig.dump(&mixed_home, None),
        fresh,
        "a forced reindex over 0.16.4's parse cache differs from a fresh index"
    );
}

/// Pass/fail criterion. Today's reader loads 0.16.4's `graph.bin` as current
/// (the bug gives a wrong or empty answer, or an `InvalidData` exit), or leaves
/// it in place and never rebuilds. The fix refuses the graph on the first read
/// (`GRAPH_FORMAT_VERSION` or `BUILDER_FINGERPRINT` differs from the writer's),
/// rebuilds once, and `find` returns the fixture symbols with their file and
/// line; the second read rebuilds nothing.
///
/// The expected rows stand for the fixture sources: the line is the line that
/// declares the symbol (`committed/<file>`), not a rendering to freeze.
///
/// Expected on HEAD: pass (format 18 against 15, schema41 against schema20).
#[test]
fn test_mixed_cache_old_graph_new_parse_cache_rebuilds_graph() {
    let rig = Rig::new();
    let today_home = rig.home("today");
    let today_repo = rig.index(&today_home, false);

    let mixed_home = rig.home("mixed");
    let repo = rig.repo_root(&mixed_home);
    if today_repo.join("parse_cache").exists() {
        copy_tree(&today_repo.join("parse_cache"), &repo.join("parse_cache"));
    }
    rig.stage_prev_commit(&repo);

    let first = rig.ecp(
        &mixed_home,
        None,
        &["find", "helper", "--all", "--format", "json"],
    );
    let refused =
        GRAPH_FORMAT_VERSION != WRITER_GRAPH_FORMAT || BUILDER_FINGERPRINT != WRITER_FINGERPRINT;
    if refused {
        assert!(
            rebuilt(&first),
            "0.16.4's graph was not refused and rebuilt: {}",
            String::from_utf8_lossy(&first.stderr)
        );
    }
    assert_eq!(hits(&first, "helper"), helper_hits());

    let second = rig.ecp(
        &mixed_home,
        None,
        &["find", "compute", "--all", "--format", "json"],
    );
    assert_eq!(
        hits(&second, "compute"),
        vec![fn_hit("compute", "pkg/core.py", 1)]
    );
    assert!(
        !rebuilt(&second),
        "the rebuilt graph was rebuilt again: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        rig.calls_from(&mixed_home, None, "run"),
        vec![json!(["lib.rs", "helper", "lib.rs"]).to_string()]
    );
}

/// Pass/fail criterion. Today's reader serves 0.16.4's V5 fragment of
/// `pkg/core.py`, or decodes its bytes as V7: `fresh_only` is then missing,
/// has another line, or the legacy files change. The fix never opens the
/// legacy namespace (`dirty_files.json`, `graph_overlay/`): the dirty file is
/// re-parsed into `l1/` as `FRAGMENT_FORMAT_V7`, `find fresh_only` returns it
/// at `pkg/core.py:9`, its `Calls` edge reaches `compute`, and the legacy
/// bytes stay as the writer left them.
///
/// Expected on HEAD: pass (C3 moved the reader to `l1/`).
#[test]
fn test_mixed_cache_new_graph_old_fragments_dirty_file_reparsed() {
    let rig = Rig::new();
    let home = rig.home("mixed");
    let repo = rig.index(&home, false);
    rig.edit();
    let session = rig.stage_prev_session(&repo, false);
    let legacy = legacy_snapshot(&session);
    assert_eq!(legacy.len(), 3, "setup: the manifest and two V5 bins");

    let (found, calls) = rig.dirty_answer(&home, SESSION);
    assert_eq!(found, fresh_only_hits());
    assert_eq!(calls, fresh_only_calls());
    assert_eq!(rig.find_all(&home, Some(SESSION), "helper"), helper_hits());

    let manifest = l1_manifest(&session);
    assert_eq!(manifest.format, DIRTY_EVENTS_FORMAT);
    assert!(manifest.from_current_builder());
    let entry = manifest
        .entries
        .get("pkg/core.py")
        .expect("pkg/core.py was re-parsed into today's manifest");
    assert_eq!(entry.format, FRAGMENT_FORMAT_V7);
    let bin = fs::read(layout::fragment_path(
        &session,
        entry.fragment_id.as_deref().unwrap(),
    ))
    .unwrap();
    assert!(
        legacy.iter().all(|(_, old)| *old != bin),
        "a V5 bin was copied into l1/fragments"
    );

    for (path, old) in &legacy {
        assert_eq!(
            &fs::read(path).unwrap(),
            old,
            "{path:?}: today's binary wrote into the legacy namespace"
        );
    }
}

/// Pass/fail criterion. Today's reader merges both manifests into one view
/// (the bug gives `find fresh_only` a hit: the old `dirty_files.json` names
/// `pkg/core.py` with an mtime the file still carries, so the legacy reader
/// would serve its V5 fragment). The fix reads `l1/dirty_events.json` only:
/// the view holds `only_today` of `app.py`, the old manifest's `fresh_only`
/// is absent, `lib.rs` and `pkg/core.py` keep their committed symbols, and
/// today's manifest names `app.py` alone (C3: `dirty_files.json` is no longer
/// read).
///
/// The test stamps `lib.rs` and `pkg/core.py` with the mtimes the old manifest
/// recorded, so the legacy reader would accept the fragments by mtime.
///
/// Expected on HEAD: pass.
#[test]
fn test_mixed_cache_old_manifest_new_dirty_events_one_view() {
    let rig = Rig::new();
    let home = rig.home("mixed");
    let repo = rig.index(&home, false);
    let session = rig.stage_prev_session(&repo, false);
    let legacy = legacy_snapshot(&session);

    let manifest = DirtyFiles::read(&layout::legacy_dirty_files_path(&session)).unwrap();
    assert_eq!(
        manifest
            .entries
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["lib.rs", "pkg/core.py"]
    );
    for (rel, entry) in &manifest.entries {
        set_mtime(&rig.wt.join(rel), entry.mtime_ns);
    }
    let mut app = String::from_utf8(committed("app.py")).unwrap();
    app.push_str("\n\ndef only_today():\n    return go()\n");
    let line = app
        .lines()
        .position(|l| l.starts_with("def only_today"))
        .unwrap() as u64
        + 1;
    fs::write(rig.wt.join("app.py"), &app).unwrap();

    assert_eq!(
        rig.find_all(&home, Some(SESSION), "only_today"),
        vec![fn_hit("only_today", "app.py", line)]
    );
    assert_eq!(
        rig.calls_from(&home, Some(SESSION), "only_today"),
        vec![json!(["app.py", "go", "app.py"]).to_string()]
    );
    assert_eq!(
        rig.find_all(&home, Some(SESSION), "fresh_only"),
        Vec::<Hit>::new(),
        "the old manifest's symbol reached the view"
    );
    assert_eq!(
        rig.find_all(&home, Some(SESSION), "run"),
        vec![fn_hit("run", "lib.rs", 5)]
    );

    let today = l1_manifest(&session);
    assert_eq!(
        today.entries.keys().map(String::as_str).collect::<Vec<_>>(),
        ["app.py"]
    );
    for (path, old) in &legacy {
        assert_eq!(&fs::read(path).unwrap(), old, "{path:?} changed");
    }
}

/// Pass/fail criterion. A view cache of another release is served to today's
/// query (the bug gives a view built from 0.16.4's rows: another line, a
/// missing edge, or a missing symbol). The fix misses (the key covers the
/// running binary; the body's `VIEW_CACHE_FORMAT` differs) and builds the view
/// again: the answer after planting 0.16.4's `view_cache.bin` equals the
/// answer before, equals the answer of a session that never had one, and is
/// the fixture's `fresh_only` at `pkg/core.py:9` calling `compute`.
///
/// A HIT cannot be tested from this fixture (README): only the miss is.
///
/// Expected on HEAD: pass.
#[test]
fn test_mixed_cache_old_view_cache_new_graph_misses() {
    let rig = Rig::new();
    let home = rig.home("mixed");
    let repo = rig.index(&home, false);
    rig.edit();

    let before = rig.dirty_answer(&home, TODAY_SESSION);
    assert_eq!(before, (fresh_only_hits(), fresh_only_calls()));

    let planted = repo
        .join("sessions")
        .join(TODAY_SESSION)
        .join("view_cache.bin");
    fs::copy(
        fixture()
            .join("ecp-home/repo/sessions")
            .join(SESSION)
            .join("view_cache.bin"),
        &planted,
    )
    .unwrap();

    assert_eq!(
        rig.dirty_answer(&home, TODAY_SESSION),
        before,
        "0.16.4's view cache changed the answer"
    );
    assert_eq!(
        rig.dirty_answer(&home, OTHER_SESSION),
        before,
        "a session with a view cache answers differently from one without"
    );
}

// ------------------------------------------------------------- edge states

/// Pass/fail criterion. A half-written `graph.bin` (today's, or 0.16.4's)
/// reaches the query: the bug gives an `rkyv` error exit or a partial graph.
/// The fix fails the structural check, rebuilds, and `find` returns the four
/// `helper` functions with their files and lines.
///
/// Expected on HEAD: pass (`header_compatible` validates the whole file).
#[test]
fn test_mixed_cache_truncated_graph_bin_rebuilds_and_answers() {
    for prev in [false, true] {
        let rig = Rig::new();
        let home = rig.home("mixed");
        let repo = if prev {
            let repo = rig.repo_root(&home);
            rig.stage_prev_commit(&repo);
            repo
        } else {
            rig.index(&home, false)
        };
        truncate_graphs(&repo);

        let out = rig.ecp(
            &home,
            None,
            &["find", "helper", "--all", "--format", "json"],
        );
        assert_eq!(hits(&out, "helper"), helper_hits(), "prev={prev}");
        assert!(
            rebuilt(&out),
            "prev={prev}: a half graph.bin was not rebuilt: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Pass/fail criterion. Piece directories that exist but hold nothing (the
/// commit dir, the parse cache dir, the session dir, `l1/fragments`) are read
/// as a graph, a cache hit or a manifest: the bug gives an I/O error exit or
/// an empty answer. The fix builds the graph, and after an edit the dirty file
/// is served from a session that starts empty.
///
/// Expected on HEAD: pass, with less confidence than the others. The commit
/// dir name equals the one the build publishes to; if the build trips on an
/// existing empty dir, this test is red and the fault is in the build's
/// publish step, not in the test.
#[test]
fn test_mixed_cache_empty_piece_dirs_rebuild_and_answer() {
    let rig = Rig::new();
    let home = rig.home("mixed");
    let repo = rig.repo_root(&home);
    for dir in [
        repo.join("commits")
            .join(format!("branch_main__{}", rig.sha)),
        repo.join("parse_cache").join(WRITER_PARSE_DIR),
        repo.join("sessions").join(SESSION),
        layout::fragments_dir(&repo.join("sessions").join(SESSION)),
    ] {
        fs::create_dir_all(dir).unwrap();
    }

    assert_eq!(rig.find_all(&home, None, "helper"), helper_hits());

    rig.edit();
    let (found, calls) = rig.dirty_answer(&home, SESSION);
    assert_eq!(found, fresh_only_hits());
    assert_eq!(calls, fresh_only_calls());
}

/// Pass/fail criterion. Two sessions of one cache, one holding 0.16.4's
/// fragments and one holding today's, share state: the bug gives one session
/// the other's symbols or rewrites the other's manifest. The fix keeps them
/// apart: both answer `fresh_only` at `pkg/core.py:9`, and a query in the old
/// session leaves today's session manifest byte for byte as it was.
///
/// Expected on HEAD: pass.
#[test]
fn test_mixed_cache_two_sessions_old_and_new_fragments_each_answer() {
    let rig = Rig::new();
    let home = rig.home("mixed");
    let repo = rig.index(&home, false);
    rig.edit();
    let old_session = rig.stage_prev_session(&repo, false);

    let expected = (fresh_only_hits(), fresh_only_calls());
    assert_eq!(rig.dirty_answer(&home, TODAY_SESSION), expected);
    let today_manifest = layout::dirty_events_path(&repo.join("sessions").join(TODAY_SESSION));
    let held = fs::read(&today_manifest).unwrap();

    assert_eq!(rig.dirty_answer(&home, SESSION), expected);
    assert_eq!(
        fs::read(&today_manifest).unwrap(),
        held,
        "a query in the old session rewrote today's session manifest"
    );
    assert!(l1_manifest(&old_session)
        .entries
        .contains_key("pkg/core.py"));
    assert_eq!(rig.dirty_answer(&home, TODAY_SESSION), expected);
}

/// Pass/fail criterion. Two `ecp find` processes start at once over a cache
/// whose graph is 0.16.4's, so both see it as stale and both try to rebuild:
/// the bug gives one of them an error exit or a partial answer. The fix
/// serialises the build (a lock), and both return the four `helper` functions.
///
/// Expected on HEAD: pass, with less confidence than the others; a red result
/// is a real race in the rebuild lock, not a test fault.
#[test]
fn test_mixed_cache_old_graph_concurrent_duplicate_call_both_answer() {
    let rig = Rig::new();
    let home = rig.home("mixed");
    rig.stage_prev_commit(&rig.repo_root(&home));

    let (a, b) = std::thread::scope(|s| {
        let args = ["find", "helper", "--all", "--format", "json"];
        let a = s.spawn(|| rig.ecp(&home, None, &args));
        let b = s.spawn(|| rig.ecp(&home, None, &args));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(hits(&a, "helper"), helper_hits());
    assert_eq!(hits(&b, "helper"), helper_hits());
}
