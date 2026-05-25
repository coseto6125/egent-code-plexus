//! Regression: an explicit non-default `--graph <path>` that does not exist
//! must fail with a clear error, NOT silently warm-attach to the cwd repo's
//! graph.
//!
//! Root cause (pre-fix): `ensure_fresh(resolve(--graph), cwd)` saw the custom
//! path as Missing and ran `attach_latest_sibling_sha(cwd)`, loading the cwd
//! repo's graph while the user believed they were querying the path they named.
//! That violates output discipline — a directed query answered against the
//! wrong graph is worse than an honest error.

use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run_in(cwd: &Path, graph: &Path) -> std::process::Output {
    Command::new(ecp_bin())
        .arg("find")
        .arg("anything")
        .arg("--graph")
        .arg(graph)
        .current_dir(cwd)
        .output()
        .expect("ecp find spawn")
}

#[test]
fn custom_graph_missing_errors_without_warm_attach() {
    // cwd is some directory; the custom --graph points at a nonexistent file
    // unrelated to cwd. Pre-fix this warm-attached to cwd's sibling graph.
    let cwd = std::env::temp_dir();
    let missing = cwd.join("definitely-not-a-real-ecp-graph-xyz.bin");
    let _ = std::fs::remove_file(&missing);

    let out = run_in(&cwd, &missing);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "expected non-zero exit for missing custom --graph; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("warm-attach"),
        "must NOT warm-attach for an explicit custom --graph; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&*missing.to_string_lossy()),
        "error should name the missing path; stderr:\n{stderr}"
    );
}
