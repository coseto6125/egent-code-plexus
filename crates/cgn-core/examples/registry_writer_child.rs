//! Test helper: opens registry at $1, upserts a `RepoAlias` with dir_name=$2,
//! marker=$3 (embedded in `common_dir` to differentiate writers).
//! Used by `tests/concurrency_registry_writers.rs` to simulate
//! N concurrent `cgn` invocations.

use cgn_core::registry::{Registry, RepoAlias};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let home_cgn = PathBuf::from(args.next().expect("arg 1: home_cgn path"));
    let repo_name = args.next().expect("arg 2: repo name");
    let marker = args.next().expect("arg 3: slot marker");

    let mut reg = Registry::open(&home_cgn).expect("registry open");
    reg.upsert_repo(RepoAlias {
        dir_name: repo_name.clone(),
        common_dir: format!("/tmp/test/{repo_name}#{marker}/.git"),
        remote_url: Some(format!("https://github.com/test/{repo_name}")),
        aliases: vec![repo_name.clone()],
        last_touched: "2026-05-17T00:00:00Z".into(),
        groups: vec![],
    })
    .expect("upsert");
}
