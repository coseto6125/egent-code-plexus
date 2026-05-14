use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run_git(repo: &Path, args: &[&str]) {
    Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
}

/// Build a temp Python repo containing a file with multiple blind-spot
/// patterns (eval/exec/dynamic-import/cross-getattr) and run `gnx analyze`
/// against it so subsequent doctor calls have a real graph.bin to read.
fn setup_repo_with_blind_spots(repo: &Path, home: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    // Two python files so top_files has >1 entry to rank.
    std::fs::write(
        repo.join("src/dispatch.py"),
        "import importlib\n\
\n\
def runtime_eval(x):\n\
    return eval(x)\n\
\n\
def runtime_exec(c):\n\
    exec(c)\n\
\n\
def dynamic_import(name):\n\
    return importlib.import_module(name)\n\
\n\
class Dispatcher:\n\
    def cross(self, other, name):\n\
        return getattr(other, name)()\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/eval_only.py"),
        "def go(s):\n    return eval(s)\n",
    )
    .unwrap();

    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/doctor-live-test.git",
        ],
    );
    run_git(repo, &["add", "-A"]);
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );

    let out = Command::new(gnx_bin())
        .args(["analyze", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("analyze failed to spawn");
    assert!(
        out.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn doctor_lists_framework_coverage() {
    let out = Command::new(gnx_bin())
        .args(["doctor"])
        .output()
        .expect("doctor failed to spawn");

    assert!(
        out.status.success(),
        "doctor exit code: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Framework coverage section present
    assert!(
        stdout.contains("framework_coverage"),
        "missing framework_coverage section\nstdout: {stdout}"
    );
    assert!(
        stdout.contains("fastapi-depends"),
        "missing fastapi-depends entry"
    );
    assert!(
        stdout.contains("django-signal-receiver"),
        "missing django-signal-receiver entry"
    );
    assert!(
        stdout.contains("axum-route-handler"),
        "missing axum-route-handler entry"
    );
    assert!(
        stdout.contains("spring-autowired"),
        "missing spring-autowired entry"
    );

    // Blind-spot catalog
    assert!(
        stdout.contains("blind_spot_catalog"),
        "missing blind_spot_catalog section"
    );
    assert!(stdout.contains("python-eval"), "missing python-eval entry");
    assert!(
        stdout.contains("python-cross-getattr"),
        "missing python-cross-getattr entry"
    );

    // Confidence thresholds
    assert!(stdout.contains("high_trust_only"), "missing threshold info");
}

#[test]
fn doctor_json_format() {
    let out = Command::new(gnx_bin())
        .args(["doctor", "--format", "json"])
        .output()
        .expect("doctor failed to spawn");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Should be valid JSON
    let _: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("doctor --format json output not valid JSON: {e}\nstdout: {stdout}")
    });

    // Contains expected keys
    assert!(stdout.contains("framework_coverage"));
    assert!(stdout.contains("blind_spot_catalog"));
}

#[test]
fn doctor_live_blind_spots_omitted_when_graph_absent() {
    // Point doctor at a graph path that does not exist; the section must
    // serialize as null (json) and be omitted from compact output so doctor
    // remains useful on a fresh checkout before the first `gnx analyze`.
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope/graph.bin");

    let out = Command::new(gnx_bin())
        .args([
            "--graph",
            missing.to_str().unwrap(),
            "doctor",
            "--format",
            "json",
        ])
        .output()
        .expect("doctor failed to spawn");
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["live_blind_spots"].is_null(),
        "expected null live_blind_spots; got {}",
        v["live_blind_spots"]
    );

    let compact = Command::new(gnx_bin())
        .args(["--graph", missing.to_str().unwrap(), "doctor"])
        .output()
        .expect("doctor compact failed to spawn");
    assert!(compact.status.success());
    let compact_out = String::from_utf8_lossy(&compact.stdout);
    assert!(
        !compact_out.contains("live_blind_spots"),
        "compact output should omit live_blind_spots when null; got:\n{compact_out}"
    );
}

#[test]
fn doctor_live_blind_spots_present_after_analyze() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    setup_repo_with_blind_spots(repo_tmp.path(), home_tmp.path());

    // JSON path: assert structure and counts.
    let out = Command::new(gnx_bin())
        .args(["doctor", "--format", "json"])
        .current_dir(repo_tmp.path())
        .env("HOME", home_tmp.path())
        .output()
        .expect("doctor failed to spawn");
    assert!(
        out.status.success(),
        "doctor failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --format json not valid JSON: {e}\nstdout: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });

    let lbs = &v["live_blind_spots"];
    assert!(
        lbs.is_object(),
        "expected live_blind_spots object, got {lbs}"
    );
    let total = lbs["total"].as_u64().expect("total must be u64");
    assert!(
        total >= 5,
        "expected >=5 blind-spot sites (2 eval + 1 exec + 1 dynamic-import + 1 cross-getattr); got total={total}"
    );

    let by_kind = lbs["by_kind"].as_object().expect("by_kind must be object");
    assert!(
        by_kind.get("python-eval").and_then(|v| v.as_u64()) == Some(2),
        "python-eval count should be 2 (one per file); got by_kind={by_kind:?}"
    );
    assert!(
        by_kind.contains_key("python-exec"),
        "missing python-exec in by_kind: {by_kind:?}"
    );
    assert!(
        by_kind.contains_key("python-dynamic-import"),
        "missing python-dynamic-import in by_kind: {by_kind:?}"
    );
    assert!(
        by_kind.contains_key("python-cross-getattr"),
        "missing python-cross-getattr in by_kind: {by_kind:?}"
    );

    let top_files = lbs["top_files"]
        .as_array()
        .expect("top_files must be array");
    assert!(
        !top_files.is_empty() && top_files.len() <= 5,
        "top_files length out of bounds: {}",
        top_files.len()
    );
    // Most-blind file is src/dispatch.py (4 patterns) — must rank first.
    let top_file = top_files[0]["file"].as_str().unwrap_or("");
    assert!(
        top_file.ends_with("src/dispatch.py"),
        "expected src/dispatch.py to rank first; top_files={top_files:?}"
    );
    // Counts must be sorted desc.
    let counts: Vec<u64> = top_files
        .iter()
        .map(|r| r["count"].as_u64().unwrap_or(0))
        .collect();
    for w in counts.windows(2) {
        assert!(
            w[0] >= w[1],
            "top_files not sorted by count desc: {counts:?}"
        );
    }

    // Compact path: section present and correctly formatted.
    let compact = Command::new(gnx_bin())
        .args(["doctor"])
        .current_dir(repo_tmp.path())
        .env("HOME", home_tmp.path())
        .output()
        .expect("doctor compact failed to spawn");
    assert!(compact.status.success());
    let compact_out = String::from_utf8_lossy(&compact.stdout);
    assert!(
        compact_out.contains("live_blind_spots:"),
        "compact missing live_blind_spots section:\n{compact_out}"
    );
    assert!(
        compact_out.contains("python-eval: 2"),
        "compact missing 'python-eval: 2' entry:\n{compact_out}"
    );
    assert!(
        compact_out.contains("src/dispatch.py"),
        "compact missing src/dispatch.py in top_files:\n{compact_out}"
    );
}
