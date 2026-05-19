//! Tests for cross-platform spawn_detached (spec §4.5).

use cgn_core::daemon::spawn_detached;

#[test]
fn detached_child_outlives_parent_call() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("child-ran");
    let marker_path = marker.to_string_lossy().into_owned();

    // cmd /C 的 quoting 規則對含 backslash 的 Windows path escape 不可靠
    // (`\"PATH\"` 會被當字面字串)，改用 PowerShell 並走單引號 path literal。
    // 單引號內的 ' 需 escape 為 ''，避免 path 真的含 ' 時破解析。
    let cmd = if cfg!(windows) {
        let ps_path = marker_path.replace('\'', "''");
        vec![
            "powershell".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!(
                "Start-Sleep -Milliseconds 200; New-Item -Force -Path '{ps_path}' -ItemType File | Out-Null"
            ),
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
    for _ in 0..30 {
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
