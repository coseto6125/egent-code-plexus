//! `ecp rename` was removed in 0.17.0: ecp does not edit files. The hidden stub
//! answers every argument list with one message and an error exit, and never
//! loads or builds an index. A caller with stale docs reads the message and
//! switches to `find` / `impact` plus its own editor.

mod common;

use common::{commit_all, ecp_bin, run_git};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const REMOVED_MESSAGE: &str = "`ecp rename` was removed in 0.17.0: ecp does not edit files. Rename with your editor's LSP rename, or list the declarations and call sites with `ecp find <name>` and `ecp impact <name>`, then edit them.";

/// A committed git repo that no `ecp` command has indexed, with its own HOME.
struct FreshRepo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl FreshRepo {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(root.join("lib.py"), "def Foo():\n    return 1\n").unwrap();
        run_git(&root, &["init", "-q", "-b", "main"]);
        commit_all(&root, "base");
        Self {
            _tmp: tmp,
            root,
            home,
        }
    }

    fn ecp(&self, args: &[&str]) -> Output {
        Command::new(ecp_bin())
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", "rename-removed")
            .env("ECP_SKIP_BG_REBUILD", "1")
            .output()
            .expect("ecp failed to spawn")
    }

    /// Every file under HOME whose name is `graph.bin`: an index the command
    /// built or loaded.
    fn built_graphs(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else if path.file_name().is_some_and(|name| name == "graph.bin") {
                    found.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.home, &mut found);
        found
    }
}

fn assert_removed(repo: &FreshRepo, out: &Output) {
    assert!(!out.status.success(), "the stub must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(REMOVED_MESSAGE), "stderr: {stderr}");
    assert_eq!(
        fs::read_to_string(repo.root.join("lib.py")).unwrap(),
        "def Foo():\n    return 1\n",
        "the stub must not edit a file"
    );
    assert_eq!(
        repo.built_graphs(),
        Vec::<PathBuf>::new(),
        "the stub must not build an index"
    );
}

#[test]
fn test_rename_removed_plain_arguments_print_message_and_build_no_index() {
    let repo = FreshRepo::new();

    let out = repo.ecp(&["rename", "Foo", "Bar"]);

    assert_removed(&repo, &out);
}

#[test]
fn test_rename_removed_old_flags_print_the_same_message() {
    let repo = FreshRepo::new();

    let out = repo.ecp(&["rename", "Foo", "Bar", "--dry-run", "--leave-unbound"]);

    assert_removed(&repo, &out);
}

#[test]
fn test_rename_removed_flags_first_print_the_same_message() {
    let repo = FreshRepo::new();

    let out = repo.ecp(&[
        "rename",
        "--dry-run",
        "--leave-unbound",
        "--symbol",
        "Foo",
        "--new-name",
        "Bar",
        "--repo",
        ".",
    ]);

    assert_removed(&repo, &out);
}

#[test]
fn test_rename_removed_stays_out_of_top_level_help() {
    let repo = FreshRepo::new();

    let out = repo.ecp(&["--help"]);

    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("rename")),
        "the hidden stub must not be listed: {help}"
    );
}
