use std::process::Command;

fn gnx_help() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .arg("--help")
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn gnx_admin_help() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["admin", "--help"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn gnx_admin_prune_help() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["admin", "prune", "--help"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn top_level_lists_nine_agent_commands() {
    let help = gnx_help();
    for cmd in [
        "inspect",
        "search",
        "impact",
        "rename",
        "cypher",
        "coverage",
        "routes",
        "scan",
        "contracts",
    ] {
        assert!(help.contains(cmd), "missing {cmd} in --help:\n{help}");
    }
}

#[test]
fn top_level_hides_admin() {
    let help = gnx_help();
    // The Admin variant exists but is hidden; "admin" should not appear
    // as a top-level command line. Allow it in descriptions.
    for line in help.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("admin ") || trimmed.starts_with("admin\t") {
            panic!("admin command leaked into top-level --help: {line}");
        }
    }
}

#[test]
fn admin_help_lists_seven_entries() {
    let help = gnx_admin_help();
    for cmd in [
        "install-hook",
        "drop",
        "prune",
        "rename-branch",
        "config",
        "group",
        "index",
    ] {
        assert!(help.contains(cmd), "missing {cmd} in admin --help:\n{help}");
    }
}

#[test]
fn admin_prune_help_keeps_orphan_sweep_mode() {
    let help = gnx_admin_prune_help();
    assert!(
        help.contains("--orphans"),
        "missing --orphans in prune help:\n{help}"
    );
}

#[test]
fn no_old_top_level_commands() {
    let help = gnx_help();
    for old in [
        "analyze",
        "context",
        "query",
        "doctor",
        "status",
        "list",
        "summarize",
        "detect-changes",
        "route-map",
        "api-impact",
        "tool-map",
        "cluster",
        "process",
        "multi-query",
        "multi_query",
        "clean",
        "remove",
        "init",
        // shape_check / shape-check intentionally surfaced at top-level
        // (PR Tier B Task 3 — drift detector is agent-facing).
        "analyze-here",
        "analyze_here",
    ] {
        for line in help.lines() {
            let trimmed = line.trim_start();
            assert!(
                !trimmed.starts_with(&format!("{old} "))
                    && !trimmed.starts_with(&format!("{old}\t")),
                "old command {old} still visible in --help"
            );
        }
    }
}
