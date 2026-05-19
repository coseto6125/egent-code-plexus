use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

#[test]
fn init_writes_hook_with_absolute_cgn_path() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo)
        .output()
        .unwrap();

    let out = Command::new(cgn_bin())
        .args(["admin", "install-hook"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin install-hook failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let hook_path = repo.join(".git/hooks/reference-transaction");
    assert!(hook_path.exists(), "expected hook at {hook_path:?}");

    let content = std::fs::read_to_string(&hook_path).unwrap();
    assert!(content.starts_with("#!/bin/sh"));
    assert!(content.contains("hook-handle"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(&hook_path).unwrap();
        assert_eq!(m.permissions().mode() & 0o111, 0o111);
    }
}
