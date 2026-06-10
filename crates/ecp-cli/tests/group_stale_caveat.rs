//! FU-2026-06-10-1f9dfc493a35: group verbs load member graphs by latest
//! mtime (`latest_graph_path_for`), so a member whose HEAD moved past its
//! indexed commit serves the old L2 silently — `ensure_fresh` refreshes the
//! L1 overlay, but queries read L2 only. The loaded engine must self-flag
//! (`behind_head`) and group output must carry a `result` caveat naming
//! WHICH member is stale, so a `found: nothing` on that member can't be
//! read as a definitive "does not exist".

use std::fs;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run_ecp(args: &[&str], home: &Path) -> std::process::Output {
    Command::new(ecp_bin())
        .args(args)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .expect("ecp spawn failed")
}

fn git(repo: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(repo)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed in {}", repo.display());
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(
        path.join("src/lib.rs"),
        "pub fn hello_world() -> &'static str { \"hello\" }\n",
    )
    .unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "init"]);
}

/// Advance HEAD one commit past the indexed SHA without reindexing.
fn advance_head(path: &Path) {
    fs::write(path.join("src/extra.rs"), "pub fn newer_fn() {}\n").unwrap();
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "advance"]);
}

/// Index both repos and put them in `group`. Returns the registry dir_name
/// of each repo, in input order (matched by basename substring).
fn index_and_group(home: &Path, repos: &[&Path], group: &str) -> Vec<String> {
    for repo in repos {
        let out = run_ecp(&["admin", "index", "--repo", repo.to_str().unwrap()], home);
        assert!(
            out.status.success(),
            "admin index failed for {}:\n{}",
            repo.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let registry: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join(".ecp/registry.json")).unwrap()).unwrap();
    let dir_names: Vec<String> = repos
        .iter()
        .map(|repo| {
            let base = repo.file_name().unwrap().to_string_lossy();
            registry["repos"]
                .as_object()
                .unwrap()
                .keys()
                .find(|k| k.contains(base.as_ref()))
                .unwrap_or_else(|| panic!("no registry entry for {base}"))
                .clone()
        })
        .collect();
    for dn in &dir_names {
        let out = run_ecp(&["admin", "group", "add", dn, group], home);
        assert!(
            out.status.success(),
            "group add failed for {dn}:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    dir_names
}

fn parse_json(out: &std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "command failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "non-JSON output ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn group_find_names_only_the_behind_head_member() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();
    let stale = repos_tmp.path().join("stalerepo");
    let fresh = repos_tmp.path().join("freshrepo");
    init_repo(&stale);
    init_repo(&fresh);
    index_and_group(home_tmp.path(), &[&stale, &fresh], "cavgrp");
    advance_head(&stale);

    let out = run_ecp(
        &["group", "find", "cavgrp", "hello", "--json"],
        home_tmp.path(),
    );
    let v = parse_json(&out);
    let caveat = v
        .get("result")
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| panic!("behind-HEAD member must produce a `result` caveat: {v}"));
    assert!(
        caveat.contains("stalerepo"),
        "caveat must name the stale member: {caveat}"
    );
    assert!(
        !caveat.contains("freshrepo"),
        "caveat must NOT implicate the fresh member: {caveat}"
    );
}

#[test]
fn group_find_rrf_carries_caveat_too() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();
    let stale = repos_tmp.path().join("stalerepo");
    init_repo(&stale);
    index_and_group(home_tmp.path(), &[&stale], "rrfcavgrp");
    advance_head(&stale);

    let out = run_ecp(
        &[
            "group",
            "find",
            "rrfcavgrp",
            "hello",
            "--merge",
            "rrf",
            "--json",
        ],
        home_tmp.path(),
    );
    let v = parse_json(&out);
    let caveat = v
        .get("result")
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| panic!("rrf output must carry the same staleness caveat: {v}"));
    assert!(
        caveat.contains("stalerepo"),
        "rrf caveat must name the stale member: {caveat}"
    );
}

#[test]
fn group_find_all_fresh_stays_caveat_free() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();
    let a = repos_tmp.path().join("repoalpha");
    let b = repos_tmp.path().join("repobeta");
    init_repo(&a);
    init_repo(&b);
    index_and_group(home_tmp.path(), &[&a, &b], "freshgrp");

    let out = run_ecp(
        &["group", "find", "freshgrp", "hello", "--json"],
        home_tmp.path(),
    );
    let v = parse_json(&out);
    assert!(
        v.get("result").is_none(),
        "all-fresh group must not pay the caveat token cost: {v}"
    );
}

#[test]
fn group_impact_behind_head_member_carries_caveat() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();
    let stale = repos_tmp.path().join("stalerepo");
    init_repo(&stale);
    let dir_names = index_and_group(home_tmp.path(), &[&stale], "impcavgrp");
    advance_head(&stale);

    let out = run_ecp(
        &[
            "group",
            "impact",
            "impcavgrp",
            "--target",
            "hello_world",
            "--repo",
            &dir_names[0],
            "--json",
        ],
        home_tmp.path(),
    );
    let v = parse_json(&out);
    let caveat = v.get("result").and_then(|c| c.as_str()).unwrap_or_else(|| {
        panic!("group impact on a behind-HEAD member must carry a `result` caveat: {v}")
    });
    assert!(
        caveat.contains(&dir_names[0]),
        "impact caveat must name the stale member: {caveat}"
    );
}
