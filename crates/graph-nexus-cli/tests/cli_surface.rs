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
        "find",
        "impact",
        "rename",
        "cypher",
        "coverage",
        "routes",
        "contracts",
        "tool-map",
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
fn admin_help_lists_expected_entries() {
    // rename-branch removed in v2 (branch not in storage)
    let help = gnx_admin_help();
    for cmd in ["install-hook", "drop", "prune", "config", "group", "index"] {
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
fn top_level_does_not_list_scan() {
    let help = gnx_help();
    for line in help.lines() {
        let t = line.trim_start();
        // scan is removed; allow the word "scan" in unrelated descriptions
        // but not as a subcommand entry (line starts with "scan " or "scan\t").
        if t.starts_with("scan ") || t.starts_with("scan\t") {
            panic!("scan command leaked into --help: {line}");
        }
    }
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
        // `tool-map` was initially folded into `coverage --externals` but
        // restored as a standalone command (the per-callsite binding
        // analysis sits beyond a health-summary's granularity).
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
