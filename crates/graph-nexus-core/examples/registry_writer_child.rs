//! Test helper: opens registry at $1, upserts a `RepoEntry` with name=$2,
//! marker=$3 (embedded in `remote_url` to differentiate writers).
//! Used by `tests/concurrency_registry_writers.rs` to simulate
//! N concurrent `gnx` invocations.

use graph_nexus_core::registry::{Registry, RepoEntry};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let home_gnx = PathBuf::from(args.next().expect("arg 1: home_gnx path"));
    let repo_name = args.next().expect("arg 2: repo name");
    let marker = args.next().expect("arg 3: slot marker");

    let mut reg = Registry::open(&home_gnx).expect("registry open");
    reg.upsert_repo(RepoEntry {
        name: repo_name.clone(),
        remote_url: format!("https://github.com/test/{repo_name}#{marker}"),
        worktree_path: format!("/tmp/test/{repo_name}"),
        index_dir_root: format!("/tmp/test/{repo_name}/.gnx"),
        branches: vec![],
        groups: vec![],
    })
    .expect("upsert");
}
