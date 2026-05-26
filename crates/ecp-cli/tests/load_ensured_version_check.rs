//! `auto_ensure::load_ensured` is the version-checked load path used by every
//! by-repo graph load (find multi-repo, group, diff) — the paths that used to
//! call `Engine::load` directly and so returned a graph built by a stale ecp
//! binary verbatim (the root cause of querying a graph still carrying an
//! already-fixed parser bug). These tests pin the two-tier contract:
//!
//!   - builder-fingerprint drift (ecp-version mismatch) → full `build_l2`
//!     BEFORE the load, so the returned engine never reflects a stale binary.
//!   - fingerprint current + tree clean → no rebuild, load the graph as-is.
//!
//! Own test binary so the `test_counters` statics aren't raced by other files.

use ecp_cli::auto_ensure::{self, test_counters};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn git_init_with_commit(p: &Path) {
    let g = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    fs::write(p.join("lib.rs"), "pub fn sentinel_fn() {}\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-qm", "init"]);
}

struct EnvSnapshot {
    home: Option<std::ffi::OsString>,
    ecp_home: Option<std::ffi::OsString>,
}

impl EnvSnapshot {
    fn take() -> Self {
        Self {
            home: std::env::var_os("HOME"),
            ecp_home: std::env::var_os("ECP_HOME"),
        }
    }
}

impl Drop for EnvSnapshot {
    fn drop(&mut self) {
        match &self.home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match &self.ecp_home {
            Some(v) => std::env::set_var("ECP_HOME", v),
            None => std::env::remove_var("ECP_HOME"),
        }
    }
}

/// Build an initial graph for `worktree` via `ensure_fresh` (Missing arm),
/// returning the REAL registry graph.bin path (under `~/.ecp/<repo>__<hash>/
/// commits/.../`), not the legacy `.ecp/graph.bin` default — load_ensured must
/// be probed against the same path the build actually wrote, else it sees
/// Missing and warm-attaches instead of detecting the fingerprint drift.
fn build_initial_graph(worktree: &Path) -> std::path::PathBuf {
    let legacy_default = Path::new(".ecp/graph.bin");
    let graph_path = ecp_cli::graph_path::resolve(legacy_default, worktree);
    auto_ensure::ensure_fresh(&graph_path, worktree).expect("initial build_l2");
    // After the build the registry exists; resolve again to the published path.
    ecp_cli::graph_path::resolve(legacy_default, worktree)
}

#[test]
fn load_ensured_rebuilds_on_fingerprint_drift() {
    let _env_guard = lock_env();
    let _snapshot = EnvSnapshot::take();

    let tmp = TempDir::new().expect("tempdir");
    let worktree = tmp.path();
    git_init_with_commit(worktree);
    std::env::set_var("HOME", worktree);
    std::env::remove_var("ECP_HOME");

    let graph_path = build_initial_graph(worktree);

    // Simulate a graph left behind by an older ecp: overwrite the builder-
    // fingerprint sidecar with a stale version string.
    let sidecar = auto_ensure::builder_fingerprint_sidecar_path(&graph_path);
    fs::write(&sidecar, "v0.0.1+schema1\n").expect("write stale sidecar");

    test_counters::reset();
    let engine = auto_ensure::load_ensured(&graph_path, worktree)
        .expect("load_ensured under fingerprint drift");

    assert_eq!(
        test_counters::build_l2_calls(),
        1,
        "fingerprint drift must force exactly one full build_l2 before the load"
    );
    // The returned engine is the freshly rebuilt graph — sentinel symbol present.
    let graph = engine.graph().expect("graph view");
    let pool = graph.string_pool.as_slice();
    assert!(
        graph
            .nodes
            .iter()
            .any(|n| n.name.resolve(pool) == "sentinel_fn"),
        "rebuilt graph must contain the source symbol"
    );
    // Drift cleared: the fingerprint sidecar write is synchronous (a stale
    // fingerprint would otherwise force a full rebuild — see
    // `write_builder_fingerprint_sidecar`), so on `load_ensured`'s return it
    // already reflects the running binary, no poll needed.
    let fp = fs::read_to_string(&sidecar).unwrap_or_default();
    assert_eq!(
        fp.trim(),
        ecp_core::registry::BUILDER_FINGERPRINT,
        "rebuild must refresh the fingerprint sidecar to the running binary"
    );
}

#[test]
fn load_ensured_no_rebuild_when_fresh() {
    let _env_guard = lock_env();
    let _snapshot = EnvSnapshot::take();

    let tmp = TempDir::new().expect("tempdir");
    let worktree = tmp.path();
    git_init_with_commit(worktree);
    std::env::set_var("HOME", worktree);
    std::env::remove_var("ECP_HOME");

    let graph_path = build_initial_graph(worktree);

    // Clean tree + current fingerprint (build_initial_graph wrote it) → Ready.
    test_counters::reset();
    let engine =
        auto_ensure::load_ensured(&graph_path, worktree).expect("load_ensured on fresh graph");

    assert_eq!(
        test_counters::build_l2_calls(),
        0,
        "a current-fingerprint, clean-tree graph must load with no rebuild"
    );
    let graph = engine.graph().expect("graph view");
    let pool = graph.string_pool.as_slice();
    assert!(
        graph
            .nodes
            .iter()
            .any(|n| n.name.resolve(pool) == "sentinel_fn"),
        "loaded graph must contain the source symbol"
    );
}
