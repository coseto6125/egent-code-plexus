//! Concurrency invariant 4.3 — Registry concurrent process writers converge.
//!
//! Real production failure mode: multiple `gnx` invocations from Claude
//! Code hooks race to upsert the registry. flock-guarded read-modify-write
//! MUST converge to a state containing every writer's contribution.

use graph_nexus_core::registry::Registry;
use std::io::Read;
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

    let mut children: Vec<_> = (0..8)
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

    for child in &mut children {
        let status = child.wait().expect("wait");
        if !status.success() {
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                let _ = s.read_to_string(&mut stderr);
            }
            panic!("child exited {}: {stderr}", status);
        }
    }

    let reg = Registry::open(&home_gnx).expect("open final");
    let snap = reg.snapshot();
    let mut names: Vec<_> = snap.repos.iter().map(|r| r.name.clone()).collect();
    names.sort();
    let expected: Vec<String> = (0..8).map(|i| format!("repo-{i:02}")).collect();
    assert_eq!(names, expected, "registry lost writes under concurrent contention");
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

    let mut children: Vec<_> = (0..8)
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

    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }

    let reg = Registry::open(&home_gnx).expect("open final");
    let snap = reg.snapshot();
    let shared: Vec<_> = snap.repos.iter().filter(|r| r.name == "shared-repo").collect();
    assert_eq!(shared.len(), 1, "duplicate or lost entry under same-key contention");
}
