use ecp_cli::commands::group::matching::match_contracts;
use ecp_cli::commands::group::types::{
    ContractRole, ContractType, ExtractedContract, MatchType, StoredContract, SymbolRef,
};
use ecp_core::config::GroupConfig;
use tempfile::TempDir;

fn make_contract(repo: &str, role: ContractRole, id: &str) -> StoredContract {
    StoredContract {
        repo: repo.into(),
        inner: ExtractedContract {
            contract_id: id.into(),
            contract_type: ContractType::Http,
            role,
            symbol_uid: format!("{repo}::{}", id.replace(':', "_")),
            symbol_ref: SymbolRef {
                file_path: "x".into(),
                name: "h".into(),
            },
            confidence: 1.0,
            service: None,
            meta: vec![],
        },
    }
}

#[test]
fn exact_match_pairs_provider_consumer() {
    let dir = TempDir::new().unwrap();
    let contracts = vec![
        make_contract("a", ContractRole::Provider, "http:GET:/x"),
        make_contract("b", ContractRole::Consumer, "http:GET:/x"),
    ];
    let cfg = GroupConfig::default();
    let (links, unmatched) = match_contracts(&contracts, dir.path(), &cfg, false).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].match_type, MatchType::Exact);
    assert!((links[0].confidence - 1.0).abs() < f32::EPSILON);
    assert!(unmatched.is_empty());
}

#[test]
fn unmatched_consumer_lands_in_unmatched() {
    let dir = TempDir::new().unwrap();
    let contracts = vec![make_contract(
        "b",
        ContractRole::Consumer,
        "http:GET:/orphan",
    )];
    let cfg = GroupConfig::default();
    let (links, unmatched) = match_contracts(&contracts, dir.path(), &cfg, true).unwrap();
    assert!(links.is_empty());
    assert_eq!(unmatched.len(), 1);
}

#[test]
fn exact_only_skips_bm25() {
    let dir = TempDir::new().unwrap();
    let contracts = vec![
        make_contract("a", ContractRole::Provider, "http:GET:/users"),
        make_contract("b", ContractRole::Consumer, "http:GET:/user"),
    ];
    let cfg = GroupConfig::default();
    let (links, unmatched) = match_contracts(&contracts, dir.path(), &cfg, true).unwrap();
    assert!(links.is_empty(), "exact_only must not BM25-match near-miss");
    assert_eq!(unmatched.len(), 1);
}

#[test]
fn exclude_paths_drops_health_check() {
    let dir = TempDir::new().unwrap();
    let contracts = vec![
        make_contract("a", ContractRole::Provider, "http:GET:/health"),
        make_contract("b", ContractRole::Consumer, "http:GET:/health"),
    ];
    let cfg = GroupConfig {
        exclude_links_paths: vec!["/health".into()],
        ..Default::default()
    };
    let (links, _) = match_contracts(&contracts, dir.path(), &cfg, false).unwrap();
    assert!(
        links.is_empty(),
        "/health must be excluded from cross-links"
    );
}

#[test]
fn bm25_matches_near_miss() {
    let dir = TempDir::new().unwrap();
    let contracts = vec![
        make_contract("a", ContractRole::Provider, "http:GET:/users"),
        make_contract("b", ContractRole::Consumer, "http:GET:/user"),
    ];
    // Lower threshold than default 0.6 to ensure near-miss matches.
    let cfg = GroupConfig {
        bm25_threshold: 0.01,
        ..Default::default()
    };
    let (links, unmatched) = match_contracts(&contracts, dir.path(), &cfg, false).unwrap();
    assert!(
        !links.is_empty(),
        "BM25 should match near-miss /users ~ /user"
    );
    assert!(
        links.iter().any(|l| l.match_type == MatchType::Bm25),
        "expected at least one Bm25-typed link; got {:?}",
        links.iter().map(|l| &l.match_type).collect::<Vec<_>>()
    );
    assert!(
        unmatched.is_empty(),
        "consumer should now be matched, not in unmatched"
    );
}
