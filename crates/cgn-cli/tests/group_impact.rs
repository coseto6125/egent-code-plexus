//! Tests for `cgn group impact`.
//!
//! Strategy:
//!
//! 1. **Unit-level cross-link traversal** — builds a `ContractRegistry` in
//!    memory (via `storage::write_contracts` + `read_contracts`), injects it
//!    into the group storage layout, then verifies that the cross_links filter
//!    works correctly when given a set of local UIDs.
//!
//! 2. **CLI smoke test** — verifies `cgn group impact --help` exits 0 and that
//!    the subcommand is wired. Full integration (2-repo index + sync + impact)
//!    is intentionally omitted because Go HTTP handler indexing surface varies
//!    (see T12 spec note on test fragility).

use cgn_cli::commands::group::{
    storage::{group_dir, read_contracts, write_contracts},
    types::{
        ContractRegistry, ContractRole, ContractType, CrossLink, CrossLinkEndpoint, ExtractedContract,
        MatchType, StoredContract, SymbolRef,
    },
};
use std::collections::HashSet;
use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn make_endpoint(repo: &str, sym_uid: &str) -> CrossLinkEndpoint {
    CrossLinkEndpoint {
        repo: repo.to_string(),
        service: None,
        symbol_uid: sym_uid.to_string(),
        symbol_ref: SymbolRef {
            file_path: format!("{repo}/main.go"),
            name: sym_uid.to_string(),
        },
    }
}

fn make_cross_link(from_repo: &str, from_uid: &str, to_repo: &str, to_uid: &str, conf: f32) -> CrossLink {
    CrossLink {
        from: make_endpoint(from_repo, from_uid),
        to: make_endpoint(to_repo, to_uid),
        contract_type: ContractType::Http,
        contract_id: format!("GET /api/{from_uid}"),
        match_type: MatchType::Exact,
        confidence: conf,
    }
}

fn make_stored_contract(repo: &str, sym_uid: &str) -> StoredContract {
    StoredContract {
        repo: repo.to_string(),
        inner: ExtractedContract {
            contract_id: format!("GET /api/{sym_uid}"),
            contract_type: ContractType::Http,
            role: ContractRole::Provider,
            symbol_uid: sym_uid.to_string(),
            symbol_ref: SymbolRef {
                file_path: format!("{repo}/main.go"),
                name: sym_uid.to_string(),
            },
            confidence: 1.0,
            service: None,
            meta: vec![],
        },
    }
}

// ── unit tests ───────────────────────────────────────────────────────────────

/// Cross-link filter: a link where `from.symbol_uid` is in local_uids should
/// be surfaced even when `to.symbol_uid` is not in local_uids.
#[test]
fn cross_links_filter_from_uid_in_local_set() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let cgn_home = home.join(".cgn");

    let registry = ContractRegistry {
        version: 1,
        contracts: vec![
            make_stored_contract("backend", "createUser"),
            make_stored_contract("frontend", "proxy"),
        ],
        cross_links: vec![
            make_cross_link("backend", "createUser", "frontend", "proxy", 0.95),
            make_cross_link("frontend", "unrelated", "backend", "other", 0.80),
        ],
        unmatched: vec![],
    };

    let gdir = group_dir(&cgn_home, "demo");
    std::fs::create_dir_all(&gdir).unwrap();
    write_contracts(&gdir, &registry).unwrap();

    let stored = read_contracts(&gdir).unwrap();

    // Simulate: local_uids contains only the backend symbol.
    let local_uids: HashSet<&str> = ["createUser"].into_iter().collect();
    let min_conf = 0.0_f32;

    let hits: Vec<_> = stored
        .cross_links
        .iter()
        .filter(|l| l.confidence >= min_conf)
        .filter(|l| {
            local_uids.contains(l.from.symbol_uid.as_str())
                || local_uids.contains(l.to.symbol_uid.as_str())
        })
        .collect();

    assert_eq!(hits.len(), 1, "expected exactly 1 cross-link hit, got: {hits:?}");
    assert_eq!(hits[0].from.repo, "backend");
    assert_eq!(hits[0].to.repo, "frontend");
}

/// Cross-link filter: `to.symbol_uid` in local_uids also triggers a hit.
#[test]
fn cross_links_filter_to_uid_in_local_set() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let cgn_home = home.join(".cgn");

    let registry = ContractRegistry {
        version: 1,
        contracts: vec![],
        cross_links: vec![
            make_cross_link("backend", "createUser", "frontend", "proxy", 0.85),
        ],
        unmatched: vec![],
    };

    let gdir = group_dir(&cgn_home, "demo");
    std::fs::create_dir_all(&gdir).unwrap();
    write_contracts(&gdir, &registry).unwrap();

    let stored = read_contracts(&gdir).unwrap();

    // Simulate: local_uids is from the frontend side.
    let local_uids: HashSet<&str> = ["proxy"].into_iter().collect();

    let hits: Vec<_> = stored
        .cross_links
        .iter()
        .filter(|l| {
            local_uids.contains(l.from.symbol_uid.as_str())
                || local_uids.contains(l.to.symbol_uid.as_str())
        })
        .collect();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].from.repo, "backend");
    assert_eq!(hits[0].to.repo, "frontend");
}

/// Confidence filter: links below min_confidence are excluded.
#[test]
fn cross_links_filter_respects_min_confidence() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let cgn_home = home.join(".cgn");

    let registry = ContractRegistry {
        version: 1,
        contracts: vec![],
        cross_links: vec![
            make_cross_link("a", "sym1", "b", "sym2", 0.4),
            make_cross_link("a", "sym1", "c", "sym3", 0.9),
        ],
        unmatched: vec![],
    };

    let gdir = group_dir(&cgn_home, "demo");
    std::fs::create_dir_all(&gdir).unwrap();
    write_contracts(&gdir, &registry).unwrap();

    let stored = read_contracts(&gdir).unwrap();
    let local_uids: HashSet<&str> = ["sym1"].into_iter().collect();
    let min_conf = 0.5_f32;

    let hits: Vec<_> = stored
        .cross_links
        .iter()
        .filter(|l| l.confidence >= min_conf)
        .filter(|l| {
            local_uids.contains(l.from.symbol_uid.as_str())
                || local_uids.contains(l.to.symbol_uid.as_str())
        })
        .collect();

    assert_eq!(hits.len(), 1, "only the high-confidence link should pass");
    assert_eq!(hits[0].to.repo, "c");
}

/// Empty local_uids yields zero cross hits even when links exist.
#[test]
fn cross_links_no_hits_when_local_uids_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let cgn_home = home.join(".cgn");

    let registry = ContractRegistry {
        version: 1,
        contracts: vec![],
        cross_links: vec![make_cross_link("a", "sym1", "b", "sym2", 0.9)],
        unmatched: vec![],
    };

    let gdir = group_dir(&cgn_home, "demo");
    std::fs::create_dir_all(&gdir).unwrap();
    write_contracts(&gdir, &registry).unwrap();

    let stored = read_contracts(&gdir).unwrap();
    let local_uids: HashSet<&str> = HashSet::new();

    let hits: Vec<_> = stored
        .cross_links
        .iter()
        .filter(|l| {
            local_uids.contains(l.from.symbol_uid.as_str())
                || local_uids.contains(l.to.symbol_uid.as_str())
        })
        .collect();

    assert!(hits.is_empty());
}

// ── CLI smoke tests ───────────────────────────────────────────────────────────

/// `cgn group impact --help` must exit 0 and mention the subcommand.
#[test]
fn group_impact_help_exits_zero() {
    let out = Command::new(cgn_bin())
        .args(["group", "impact", "--help"])
        .output()
        .expect("cgn spawn failed");

    assert!(
        out.status.success(),
        "expected exit 0 for --help, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("target") && stdout.contains("repo"),
        "help text should mention --target and --repo:\n{stdout}"
    );
}

/// Unknown group → non-zero exit with a useful error.
#[test]
fn group_impact_unknown_group_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();

    let out = Command::new(cgn_bin())
        .args([
            "group", "impact", "__no_such_group__",
            "--target", "foo",
            "--repo", "bar",
        ])
        .env("HOME", tmp.path())
        .output()
        .expect("cgn spawn failed");

    assert!(
        !out.status.success(),
        "expected non-zero exit for unknown group"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("group"),
        "error message should mention group: {stderr}"
    );
}
