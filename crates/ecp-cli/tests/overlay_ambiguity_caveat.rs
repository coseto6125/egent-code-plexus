//! A dirty file whose base declarations the overlay cannot pair by identity
//! (a same-name overload set has no `Signature` discriminator yet) hides them
//! and their edges. Every graph command that reads the view says so in the
//! payload's `result` field, names the hidden declarations, and says it as
//! well on a view-cache hit. A normal deletion says nothing: it is not a gap.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const K_JAVA: &str = "public class K {\n    public void save(int n) {\n    }\n\n    public void save(String s) {\n    }\n}\n";
const CALLER_JAVA: &str =
    "public class Caller {\n    public void run(K k) {\n        k.save(1);\n    }\n}\n";
const U_JAVA: &str =
    "public class U {\n    public void load() {\n    }\n\n    public void store() {\n    }\n}\n";

/// The line a consumer matches on: the overlay could not pair a declaration.
const GAP_MARKER: &str = "could not pair";

/// An indexed git repo holding `K.java` (two `save` overloads), `Caller.java`
/// (calls `K.save`) and `U.java` (two unique methods), with its own cache root.
struct JavaRepo {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    session_id: &'static str,
}

impl JavaRepo {
    fn new(session_id: &'static str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(&home).unwrap();
        for (rel, text) in [
            ("src/K.java", K_JAVA),
            ("src/Caller.java", CALLER_JAVA),
            ("src/U.java", U_JAVA),
        ] {
            fs::write(repo.join(rel), text).unwrap();
        }
        let fixture = Self {
            _tmp: tmp,
            repo,
            home,
            session_id,
        };
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture.git(&["config", "user.email", "test@example.invalid"]);
        fixture.git(&["config", "user.name", "test"]);
        fixture.git(&["add", "-A"]);
        fixture.git(&["commit", "-qm", "base"]);
        let index = fixture
            .ecp_command(&fixture.repo)
            .args(["admin", "index", "--repo", fixture.repo.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            index.status.success(),
            "admin index: {}",
            String::from_utf8_lossy(&index.stderr)
        );
        fixture
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }

    /// `ecp` run in `cwd`, with the cache root and the session pinned to the
    /// fixture, as an agent host runs it.
    fn ecp_command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ecp"));
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", self.session_id)
            .env("ECP_SKIP_BG_REBUILD", "1");
        command
    }

    fn json_in(&self, cwd: &Path, args: &[&str]) -> Value {
        let out = self
            .ecp_command(cwd)
            .args(args)
            .args(["--format", "json"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "ecp {args:?}: {}\n{stdout}",
            String::from_utf8_lossy(&out.stderr)
        );
        let start = stdout
            .find('{')
            .unwrap_or_else(|| panic!("no JSON in stdout: {stdout}"));
        serde_json::from_str(&stdout[start..]).unwrap()
    }

    /// The `result` caveat of `ecp <args> --format json` run in the repo;
    /// empty when the payload carries none.
    fn result(&self, args: &[&str]) -> String {
        result_of(&self.json_in(&self.repo, args))
    }

    /// Comment-only edit: the parse holds every declaration the base holds.
    fn touch_comment(&self, rel: &str) {
        let path = self.repo.join(rel);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(path, format!("{text}// comment-only edit\n")).unwrap();
    }
}

fn result_of(payload: &Value) -> String {
    payload["result"].as_str().unwrap_or_default().to_string()
}

/// The contract the string stands in for: the caveat names the hidden group
/// by owner.name, path and declaration count, so a consumer can tell which
/// declarations an empty answer cannot speak for.
fn assert_names_hidden_save(result: &str) {
    assert!(result.contains(GAP_MARKER), "no gap line: {result:?}");
    for part in ["K.save", "src/K.java", "x2"] {
        assert!(result.contains(part), "missing {part:?}: {result:?}");
    }
}

/// Stands for: a comment-only edit of `K.java` leaves `save(int)` and
/// `save(String)` unpairable, and the answer says which declarations it
/// hides instead of returning a clean-looking result. A second identical
/// query reads the view from the cache and says the same.
#[test]
fn test_find_dirty_java_overloads_result_names_hidden_declarations() {
    let fx = JavaRepo::new("rpib-java-overloads");
    assert!(
        !fx.result(&["find", "save"]).contains(GAP_MARKER),
        "setup: a clean tree has no gap"
    );

    fx.touch_comment("src/K.java");
    let first = fx.result(&["find", "save"]);
    let second = fx.result(&["find", "save"]);

    assert_names_hidden_save(&first);
    assert_eq!(second, first, "a view-cache hit must carry the same caveat");
}

/// Stands for: deleting a unique method is a normal deletion, not a pairing
/// gap, so the answer carries no such line.
#[test]
fn test_find_dirty_unique_method_deleted_no_ambiguity_caveat() {
    let fx = JavaRepo::new("rpib-unique-deleted");
    let kept = "public class U {\n    public void load() {\n    }\n}\n";
    fs::write(fx.repo.join("src/U.java"), kept).unwrap();

    let kept_result = fx.result(&["find", "load"]);
    let deleted_result = fx.result(&["find", "store"]);

    assert!(!kept_result.contains(GAP_MARKER), "{kept_result:?}");
    assert!(!deleted_result.contains(GAP_MARKER), "{deleted_result:?}");
}

/// Stands for: the by-repo path builds its own caveat and must not drop the
/// overlay line. `--repo @all` from a directory that is not the repo answers
/// from the registry entry.
#[test]
fn test_find_by_repo_dirty_overloads_result_names_hidden_declarations() {
    let fx = JavaRepo::new("rpib-by-repo");
    fx.touch_comment("src/K.java");

    let payload = fx.json_in(
        &fx.home,
        &["find", "save", "--mode", "bm25", "--repo", "@all"],
    );

    assert_names_hidden_save(&result_of(&payload));
}

/// Stands for: `impact --baseline` answers from the same view, so an empty or
/// partial `impact_by_symbol` carries the line and cannot read as "no
/// callers".
#[test]
fn test_impact_baseline_dirty_overloads_carries_caveat() {
    let fx = JavaRepo::new("rpib-impact-baseline");
    fx.touch_comment("src/K.java");

    let result = fx.result(&["impact", "--baseline", "HEAD", "--repo", "."]);

    assert_names_hidden_save(&result);
}

/// Stands for: `routes` answers from the base graph alone, so a pairing gap
/// in the overlay hides nothing from it and its answer carries no such line.
#[test]
fn test_routes_dirty_overloads_no_ambiguity_caveat() {
    let fx = JavaRepo::new("rpib-routes");
    fx.touch_comment("src/K.java");

    let result = fx.result(&["routes"]);

    assert!(!result.contains(GAP_MARKER), "{result:?}");
}

/// Stands for: an `impact --batch` miss under a hidden set is no proof of
/// absence either, so the error payload carries the same line as a hit.
#[test]
fn test_impact_batch_dirty_overloads_error_payload_carries_caveat() {
    let fx = JavaRepo::new("rpib-impact-batch");
    fx.touch_comment("src/K.java");

    let mut child = fx
        .ecp_command(&fx.repo)
        .args(["impact", "--batch", "--repo", ".", "--format", "json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"no_such_symbol\n").unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout: {stdout}"));
    let payload: Value = serde_json::Deserializer::from_str(&stdout[start..])
        .into_iter()
        .next()
        .unwrap()
        .unwrap();

    assert_eq!(payload["status"], "not_found", "{payload}");
    assert_names_hidden_save(&result_of(&payload));
}
