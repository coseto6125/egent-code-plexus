use ecp_core::peer::retention::{rotate_if_needed, MSG_LOG_KEEP_ROTATED};
use std::fs;
use tempfile::tempdir;

#[test]
fn no_rotate_when_under_threshold() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("msg.log");
    fs::write(&log, b"small").unwrap();
    let rotated = rotate_if_needed(&log, 1024 * 1024, MSG_LOG_KEEP_ROTATED).unwrap();
    assert!(!rotated);
    assert!(log.exists());
    assert!(!dir.path().join("msg.log.1").exists());
}

#[test]
fn rotates_when_over_threshold_and_chains_files() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("msg.log");
    fs::write(&log, vec![b'x'; 100]).unwrap();
    let rotated = rotate_if_needed(&log, 50, MSG_LOG_KEEP_ROTATED).unwrap();
    assert!(rotated);
    assert!(log.exists() && fs::metadata(&log).unwrap().len() == 0);
    assert!(dir.path().join("msg.log.1").exists());
    assert_eq!(
        fs::metadata(dir.path().join("msg.log.1")).unwrap().len(),
        100,
        "rotated file preserves original content"
    );
}

#[test]
fn rotation_drops_oldest_beyond_keep_count() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("msg.log");
    for n in 1..=MSG_LOG_KEEP_ROTATED {
        fs::write(dir.path().join(format!("msg.log.{n}")), format!("rot{n}")).unwrap();
    }
    fs::write(&log, vec![b'y'; 100]).unwrap();
    rotate_if_needed(&log, 50, MSG_LOG_KEEP_ROTATED).unwrap();
    assert!(
        !dir.path()
            .join(format!("msg.log.{}", MSG_LOG_KEEP_ROTATED + 1))
            .exists(),
        "must not create msg.log.{}",
        MSG_LOG_KEEP_ROTATED + 1
    );
    assert!(dir.path().join("msg.log.1").exists());
    assert!(dir
        .path()
        .join(format!("msg.log.{}", MSG_LOG_KEEP_ROTATED))
        .exists());
}

#[test]
fn rotate_missing_file_is_noop() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("absent.log");
    let rotated = rotate_if_needed(&log, 50, 7).unwrap();
    assert!(!rotated);
}
