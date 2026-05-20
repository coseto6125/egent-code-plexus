//! Tests for cross-platform spawn_detached (spec §4.5).

use ecp_core::daemon::spawn_detached;
use std::path::PathBuf;
use std::process::Command;

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
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mut path = target_dir
        .join(profile)
        .join("examples")
        .join("detached_marker_child");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    if !path.exists() {
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "-p",
                "ecp-core",
                "--example",
                "detached_marker_child",
            ])
            .status()
            .expect("spawn cargo build --example detached_marker_child");
        assert!(
            status.success(),
            "cargo build --example detached_marker_child failed"
        );
    }
    path
}

#[test]
fn detached_child_outlives_parent_call() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("child-ran");
    let marker_path = marker.to_string_lossy().into_owned();
    let child = example_path();
    let child_path = child.to_string_lossy().into_owned();
    let cmd = [child_path.as_str(), marker_path.as_str()];

    spawn_detached(&cmd).unwrap();

    // Wait for the marker (poll with timeout)
    let mut found = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if marker.exists() {
            found = true;
            break;
        }
    }
    assert!(found, "expected detached child to create marker file");
}

#[test]
fn empty_argv_returns_error() {
    let r = spawn_detached(&[]);
    assert!(r.is_err());
}
