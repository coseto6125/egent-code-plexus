//! Pin the contract that `ecp find` in fuzzy mode exposes how many
//! candidates live in Test files and got silently filtered out.
//!
//! `find.rs` excludes Test-category files from fuzzy results unless
//! `--include-tests` is set. An LLM doing rename / impact analysis sees
//! a smaller candidate set than reality and can wrongly infer that a
//! symbol has no test coverage or is dead. The `tests_excluded` field
//! surfaces the omission so the LLM can branch on it.
//!
//! Contract: payload always carries `tests_excluded` (u32). It is 0 in
//! Exact mode (where the filter is bypassed) and equal to the number of
//! Test-file matches dropped in Fuzzy mode without `--include-tests`.

mod common;

use common::{ecp_bin, init_and_analyze, write};

use serde_json::Value;
use std::process::Command;

#[test]
fn fuzzy_find_surfaces_test_file_exclusions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // Substring `helper`: one match in src/, two in tests/. Fuzzy mode
    // without --include-tests must drop the test-file hits silently
    // today; the contract now requires it to surface the count.
    write(repo, "src/lib.rs", "pub fn helper_main() {}\n");
    write(
        repo,
        "tests/integration_test.rs",
        "#[test]\nfn helper_one() {}\n",
    );
    write(
        repo,
        "tests/another_test.rs",
        "#[test]\nfn helper_two() {}\n",
    );
    init_and_analyze(repo);

    let out = Command::new(ecp_bin())
        .args([
            "find", "helper", "--mode", "fuzzy", "--format", "json", "--repo", ".",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp find failed to spawn");
    assert!(
        out.status.success(),
        "ecp find failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result: Value =
        serde_json::from_slice(&out.stdout).expect("ecp find produced non-JSON output");

    let tests_excluded = result["tests_excluded"]
        .as_u64()
        .expect("payload must carry `tests_excluded`");

    assert!(
        tests_excluded >= 2,
        "expected >=2 test-file hits excluded (helper_one, helper_two), got {tests_excluded}: {result}"
    );
}
