use cgn_cli::commands::group::storage::{self, group_dir};
use cgn_cli::commands::group::types::*;
use std::path::Path;
use tempfile::TempDir;

fn seed(home: &Path) {
    let home_cgn = home.join(".cgn");
    let gdir = group_dir(&home_cgn, "demo");
    std::fs::create_dir_all(&gdir).unwrap();

    let p_http = StoredContract {
        repo: "a".into(),
        inner: ExtractedContract {
            contract_id: "http:GET:/x".into(),
            contract_type: ContractType::Http,
            role: ContractRole::Provider,
            symbol_uid: "a::p_http".into(),
            symbol_ref: SymbolRef {
                file_path: "x".into(),
                name: "p_http".into(),
            },
            confidence: 1.0,
            service: None,
            meta: vec![],
        },
    };
    let c_http = StoredContract {
        repo: "b".into(),
        inner: ExtractedContract {
            contract_id: "http:GET:/x".into(),
            contract_type: ContractType::Http,
            role: ContractRole::Consumer,
            symbol_uid: "b::c_http".into(),
            symbol_ref: SymbolRef {
                file_path: "y".into(),
                name: "c_http".into(),
            },
            confidence: 1.0,
            service: None,
            meta: vec![],
        },
    };
    let p_grpc = StoredContract {
        repo: "a".into(),
        inner: ExtractedContract {
            contract_id: "grpc:UserService:*".into(),
            contract_type: ContractType::Grpc,
            role: ContractRole::Provider,
            symbol_uid: "a::p_grpc".into(),
            symbol_ref: SymbolRef {
                file_path: "x".into(),
                name: "p_grpc".into(),
            },
            confidence: 0.9,
            service: None,
            meta: vec![],
        },
    };
    let c_grpc = StoredContract {
        repo: "c".into(),
        inner: ExtractedContract {
            contract_id: "grpc:UnrelatedService:*".into(),
            contract_type: ContractType::Grpc,
            role: ContractRole::Consumer,
            symbol_uid: "c::c_grpc".into(),
            symbol_ref: SymbolRef {
                file_path: "z".into(),
                name: "c_grpc".into(),
            },
            confidence: 0.9,
            service: None,
            meta: vec![],
        },
    };
    let link = CrossLink {
        from: CrossLinkEndpoint {
            repo: "b".into(),
            service: None,
            symbol_uid: "b::c_http".into(),
            symbol_ref: SymbolRef {
                file_path: "y".into(),
                name: "c_http".into(),
            },
        },
        to: CrossLinkEndpoint {
            repo: "a".into(),
            service: None,
            symbol_uid: "a::p_http".into(),
            symbol_ref: SymbolRef {
                file_path: "x".into(),
                name: "p_http".into(),
            },
        },
        contract_type: ContractType::Http,
        contract_id: "http:GET:/x".into(),
        match_type: MatchType::Exact,
        confidence: 1.0,
    };
    let reg = ContractRegistry {
        version: 1,
        contracts: vec![p_http, c_http, p_grpc, c_grpc],
        cross_links: vec![link],
        unmatched: vec![],
    };
    storage::write_contracts(&gdir, &reg).unwrap();

    // Write minimal registry.json to $HOME/.cgn/
    let reg_json = serde_json::json!({
        "version": 2,
        "repos": {},
        "groups": [{"name": "demo", "members": ["a", "b", "c"]}]
    });
    std::fs::write(
        home_cgn.join("registry.json"),
        serde_json::to_vec_pretty(&reg_json).unwrap(),
    )
    .unwrap();
}

#[test]
fn contracts_lists_all_when_no_filters() {
    let home = TempDir::new().unwrap();
    seed(home.path());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cgn"))
        .env("HOME", home.path())
        .args(["group", "contracts", "demo", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["contracts"].as_array().unwrap().len(), 4);
}

#[test]
fn contracts_unmatched_only_filters_matched_out() {
    let home = TempDir::new().unwrap();
    seed(home.path());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cgn"))
        .env("HOME", home.path())
        .args(["group", "contracts", "demo", "--unmatched", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let contracts = v["contracts"].as_array().unwrap();
    // p_http and c_http are matched (via cross_link) → excluded
    // p_grpc and c_grpc are NOT in cross_links → included
    assert_eq!(contracts.len(), 2);
    assert!(contracts
        .iter()
        .all(|c| c["matched"].as_bool() == Some(false)));
}

#[test]
fn contracts_type_http_filters_by_type() {
    let home = TempDir::new().unwrap();
    seed(home.path());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cgn"))
        .env("HOME", home.path())
        .args(["group", "contracts", "demo", "--type", "http", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let contracts = v["contracts"].as_array().unwrap();
    assert_eq!(contracts.len(), 2);
    for c in contracts {
        assert_eq!(c["contract_type"].as_str(), Some("http"));
    }
}

#[test]
fn contracts_repo_filter() {
    let home = TempDir::new().unwrap();
    seed(home.path());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cgn"))
        .env("HOME", home.path())
        .args(["group", "contracts", "demo", "--repo", "a", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let contracts = v["contracts"].as_array().unwrap();
    assert_eq!(contracts.len(), 2);
    for c in contracts {
        assert_eq!(c["repo"].as_str(), Some("a"));
    }
}
