use graph_nexus_cli::commands::group::storage::{
    read_contracts, read_contracts_archived, write_contracts, GroupMeta, RepoSnapshot,
};
use graph_nexus_cli::commands::group::types::{
    ContractRegistry, ContractRole, ContractType, ExtractedContract,
    MatchType, StoredContract, SymbolRef, CrossLink, CrossLinkEndpoint,
};
use std::collections::BTreeMap;
use tempfile::TempDir;

fn sample_registry() -> ContractRegistry {
    let provider = StoredContract {
        repo: "backend".into(),
        inner: ExtractedContract {
            contract_id: "http:POST:/api/users".into(),
            contract_type: ContractType::Http,
            role: ContractRole::Provider,
            symbol_uid: "backend::handlers::create_user".into(),
            symbol_ref: SymbolRef {
                file_path: "src/handlers.rs".into(),
                name: "create_user".into(),
            },
            confidence: 1.0,
            service: None,
            meta: vec![("method".into(), "POST".into())],
        },
    };
    let consumer = StoredContract {
        repo: "frontend".into(),
        inner: ExtractedContract {
            contract_id: "http:POST:/api/users".into(),
            contract_type: ContractType::Http,
            role: ContractRole::Consumer,
            symbol_uid: "frontend::api::createUser".into(),
            symbol_ref: SymbolRef {
                file_path: "src/api.ts".into(),
                name: "createUser".into(),
            },
            confidence: 1.0,
            service: None,
            meta: vec![],
        },
    };
    let link = CrossLink {
        from: CrossLinkEndpoint {
            repo: "frontend".into(),
            service: None,
            symbol_uid: consumer.inner.symbol_uid.clone(),
            symbol_ref: consumer.inner.symbol_ref.clone(),
        },
        to: CrossLinkEndpoint {
            repo: "backend".into(),
            service: None,
            symbol_uid: provider.inner.symbol_uid.clone(),
            symbol_ref: provider.inner.symbol_ref.clone(),
        },
        contract_type: ContractType::Http,
        contract_id: provider.inner.contract_id.clone(),
        match_type: MatchType::Exact,
        confidence: 1.0,
    };
    ContractRegistry {
        version: 1,
        contracts: vec![provider, consumer],
        cross_links: vec![link],
        unmatched: vec![],
    }
}

#[test]
fn contracts_rkyv_roundtrip_preserves_all_fields() {
    let dir = TempDir::new().unwrap();
    let registry = sample_registry();
    write_contracts(dir.path(), &registry).unwrap();
    let read = read_contracts(dir.path()).unwrap();
    assert_eq!(read.contracts.len(), 2);
    assert_eq!(read.cross_links.len(), 1);
    assert_eq!(read.cross_links[0].contract_id, "http:POST:/api/users");
    assert_eq!(read.cross_links[0].match_type, MatchType::Exact);
}

#[test]
fn meta_json_roundtrip() {
    let dir = TempDir::new().unwrap();
    let mut snapshots = BTreeMap::new();
    snapshots.insert(
        "backend".into(),
        RepoSnapshot {
            indexed_at: "2026-05-18T10:00:00Z".into(),
            last_commit: "abc123".into(),
        },
    );
    let meta = GroupMeta {
        version: 1,
        generated_at: "2026-05-18T10:05:00Z".into(),
        repo_snapshots: snapshots,
        missing_repos: vec!["legacy".into()],
        config_source: "default".into(),
    };
    graph_nexus_cli::commands::group::storage::write_meta(dir.path(), &meta).unwrap();
    let read = graph_nexus_cli::commands::group::storage::read_meta(dir.path()).unwrap();
    assert_eq!(read.generated_at, meta.generated_at);
    assert_eq!(read.missing_repos, vec!["legacy".to_string()]);
}

#[test]
fn read_contracts_missing_returns_empty() {
    let dir = TempDir::new().unwrap();
    let read = read_contracts(dir.path()).unwrap();
    assert!(read.contracts.is_empty());
    assert!(read.cross_links.is_empty());
}

#[test]
fn contracts_rkyv_archived_zero_copy_read() {
    let dir = TempDir::new().unwrap();
    let registry = sample_registry();
    write_contracts(dir.path(), &registry).unwrap();
    let handle = read_contracts_archived(dir.path()).unwrap();
    let arch = handle.archived().unwrap();
    assert_eq!(arch.contracts.len(), 2);
    assert_eq!(arch.contracts[0].inner.contract_id.as_str(), "http:POST:/api/users");
    assert_eq!(arch.cross_links[0].contract_id.as_str(), "http:POST:/api/users");
}
