//! Tests for cross-platform spawn_detached (spec §4.5).

use cgn_core::daemon::spawn_detached;

#[test]
fn detached_child_outlives_parent_call() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("child-ran");
    let marker_path = marker.to_string_lossy().into_owned();

    let cmd = if cfg!(windows) {
        let cmd_path = marker_path.replace('"', r#"\""#);
        vec![
            "cmd".to_string(),
            "/C".to_string(),
            format!(r#"ping -n 2 127.0.0.1 >NUL & type NUL > "{cmd_path}""#),
        ]
    } else {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("sleep 0.2; touch \"{marker_path}\""),
        ]
    };

    let args: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    spawn_detached(&args).unwrap();

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
