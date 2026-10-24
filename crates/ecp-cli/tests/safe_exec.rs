//! Tests for safe_exec hardening wrapper (spec §8 H4).

use ecp_cli::git::safe_exec;

#[test]
fn safe_exec_disables_protocol_ext() {
    let cmd = safe_exec::git();
    let args: Vec<String> = cmd.get_args().map(|s| s.to_string_lossy().into()).collect();
    assert!(
        args.iter().any(|a| a == "protocol.ext.allow=never"),
        "expected protocol.ext.allow=never in {args:?}"
    );
}

#[test]
fn safe_exec_disables_fsmonitor_editor_credential() {
    let cmd = safe_exec::git();
    let args: Vec<String> = cmd.get_args().map(|s| s.to_string_lossy().into()).collect();
    assert!(args.iter().any(|a| a == "core.fsmonitor="));
    assert!(args.iter().any(|a| a == "core.editor=false"));
    assert!(args.iter().any(|a| a == "credential.helper="));
}

#[cfg(unix)]
mod filter_overrides {
    use ecp_cli::git::safe_exec::{self, FilterOverrides};
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::{Command, ExitStatus, Output};

    fn config_output(code: i32, stdout: &[u8]) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: b"fatal: bad config line 1".to_vec(),
        }
    }

    fn args(cmd: &Command) -> Vec<Vec<u8>> {
        cmd.get_args().map(|a| a.as_bytes().to_vec()).collect()
    }

    #[test]
    fn test_from_config_output_no_match_empty_and_adds_no_args() {
        let overrides = FilterOverrides::from_config_output(&config_output(1, b"")).unwrap();
        assert!(overrides.is_empty());
        let dir = Path::new(".");
        assert_eq!(
            args(&safe_exec::git_with_overrides(dir, &overrides)),
            args(&safe_exec::git())
        );
    }

    /// A config git cannot read is an error, never an empty set: the guarded
    /// command would otherwise run with no overrides at all.
    #[test]
    fn test_from_config_output_failure_errors() {
        assert!(FilterOverrides::from_config_output(&config_output(128, b"")).is_err());
        assert!(FilterOverrides::from_config_output(&config_output(3, b"")).is_err());
        assert!(
            FilterOverrides::from_config_output(&config_output(1, b"filter.x.clean\0")).is_err()
        );
    }

    #[test]
    fn test_from_config_output_dotted_and_repeated_names_one_entry_each() {
        let out = config_output(
            0,
            b"filter.a.b.clean\0filter.a.b.process\0filter.lfs.smudge\0filter.lfs.required\0",
        );
        let overrides = FilterOverrides::from_config_output(&out).unwrap();
        let mut cmd = Command::new("git");
        overrides.apply(&mut cmd);
        let rendered: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            [
                "-c",
                "filter.a.b.clean=",
                "-c",
                "filter.a.b.smudge=",
                "-c",
                "filter.a.b.process=",
                "-c",
                "filter.a.b.required=false",
                "-c",
                "filter.lfs.clean=",
                "-c",
                "filter.lfs.smudge=",
                "-c",
                "filter.lfs.process=",
                "-c",
                "filter.lfs.required=false",
            ]
        );
    }

    #[test]
    fn test_filter_overrides_unreadable_repo_config_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-q"]);
        fs::write(repo.join(".git/config"), "[core\n").unwrap();
        assert!(safe_exec::filter_overrides(repo).is_err());
    }

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Checks `f.txt` out through a driver named `name`, first unguarded to
    /// prove the driver fires, then through [`safe_exec::git_with_overrides`].
    fn assert_checkout_never_runs_driver(name: &[u8]) {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("f.txt"), "a\n").unwrap();
        fs::write(
            repo.join(".gitattributes"),
            [&b"f.txt filter="[..], name, b"\n"].concat(),
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "one"]);
        let marker = root.join("filter-ran");
        let script = root.join("filter.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf ran > '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let program = script.as_os_str().as_bytes();
        let mut config = fs::read(repo.join(".git/config")).unwrap();
        for leaf in [&b"smudge"[..], b"process"] {
            config.extend_from_slice(b"[filter \"");
            config.extend_from_slice(name);
            config.extend_from_slice(b"\"]\n\t");
            config.extend_from_slice(leaf);
            config.extend_from_slice(b" = ");
            config.extend_from_slice(program);
            config.extend_from_slice(b"\n\trequired = true\n");
        }
        fs::write(repo.join(".git/config"), config).unwrap();

        fs::remove_file(repo.join("f.txt")).unwrap();
        let _ = safe_exec::git()
            .args(["checkout", "--", "f.txt"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            marker.exists(),
            "{:?} control: an unguarded checkout did not run the driver",
            OsStr::from_bytes(name)
        );
        fs::remove_file(&marker).unwrap();

        let _ = fs::remove_file(repo.join("f.txt"));
        let overrides = safe_exec::filter_overrides(&repo).unwrap();
        let out = safe_exec::git_with_overrides(&repo, &overrides)
            .args(["checkout", "--", "f.txt"])
            .output()
            .unwrap();
        assert!(
            !marker.exists(),
            "{:?} the guarded checkout ran the driver",
            OsStr::from_bytes(name)
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(fs::read(repo.join("f.txt")).unwrap(), b"a\n");
    }

    #[test]
    fn test_git_with_overrides_dotted_name_never_runs_driver() {
        assert_checkout_never_runs_driver(b"a.b.evil");
    }

    /// `-c filter.x=y.smudge=` sets `filter.x` and leaves `x=y` live.
    #[test]
    fn test_git_with_overrides_name_with_equals_never_runs_driver() {
        assert_checkout_never_runs_driver(b"x=y");
    }

    #[test]
    fn test_git_with_overrides_non_utf8_name_never_runs_driver() {
        assert_checkout_never_runs_driver(b"ev\xffil");
    }
}
