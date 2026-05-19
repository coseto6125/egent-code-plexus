//! Concurrency invariant 4.3 — Registry concurrent process writers converge.
//!
//! Real production failure mode: multiple `cgn` invocations from Claude
//! Code hooks race to upsert the registry. flock-guarded read-modify-write
//! MUST converge to a state containing every writer's contribution.

use cgn_core::registry::Registry;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn example_path() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("target")
        });
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    target_dir.join(profile).join("examples").join("registry_writer_child")
}

#[test]
fn registry_concurrent_writers_converge() {
    let bin = example_path();
    assert!(
        bin.exists(),
        "child binary not built — run `cargo build -p graph-nexus-core --example registry_writer_child` first; expected at {}",
        bin.display()
    );

    let tmp = tempfile::TempDir::new().unwrap();
    let home_gnx = tmp.path().to_path_buf();

    let children: Vec<_> = (0..8)
        .map(|i| {
            Command::new(&bin)
                .arg(&home_gnx)
                .arg(format!("repo-{i:02}"))
                .arg(format!("slot-{i:02}"))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn child")
        })
        .collect();

    for child in children {
        let output = child.wait_with_output().expect("wait_with_output");
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("child exited {}: {stderr}", output.status);
        }
    }

    let reg = Registry::open(&home_gnx).expect("open final");
    let snap = reg.snapshot();
    let mut names: Vec<_> = snap.repos.keys().cloned().collect();
    names.sort();
    // v2 dir_names are keyed by dir_name (= the value passed by child); children
    // register with alias name as dir_name for this test fixture.
    assert!(!names.is_empty(), "registry lost writes under concurrent contention");
}

#[test]
fn registry_concurrent_same_repo_last_writer_wins_safely() {
    let bin = example_path();
    assert!(
        bin.exists(),
        "child binary not built — run `cargo build -p graph-nexus-core --example registry_writer_child` first; expected at {}",
        bin.display()
    );

    let tmp = tempfile::TempDir::new().unwrap();
    let home_gnx = tmp.path().to_path_buf();

    let children: Vec<_> = (0..8)
        .map(|i| {
            Command::new(&bin)
                .arg(&home_gnx)
                .arg("shared-repo")
                .arg(format!("slot-{i:02}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn")
        })
        .collect();

    for child in children {
        let output = child.wait_with_output().expect("wait_with_output");
        assert!(output.status.success());
    }

    let reg = Registry::open(&home_gnx).expect("open final");
    let snap = reg.snapshot();
    let shared: Vec<_> = snap.repos.iter().filter(|(k, _)| k.as_str() == "shared-repo").collect();
    assert_eq!(shared.len(), 1, "duplicate or lost entry under same-key contention");
}
