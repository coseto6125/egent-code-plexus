//! `gnx group contracts <name> [--type T] [--repo R] [--unmatched] [--json]`
//! Lists contracts with optional filtering by type, repo, and match status.

use clap::Args;
use graph_nexus_core::registry::resolve_home_gnx;
use graph_nexus_core::GnxError;
use std::collections::HashSet;

use crate::commands::group::storage;
use crate::commands::group::types::{ContractRole, ContractType};
use serde_json::json;

#[derive(Args, Debug, Clone)]
pub struct ContractsArgs {
    /// Group name.
    pub name: String,
    /// Filter by contract type (http|grpc|thrift|topic|lib|include|custom).
    #[arg(long, value_parser = parse_type)]
    pub r#type: Option<ContractType>,
    /// Filter by repo name.
    #[arg(long)]
    pub repo: Option<String>,
    /// Show only unmatched contracts.
    #[arg(long)]
    pub unmatched: bool,
    /// Emit JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

fn parse_type(s: &str) -> Result<ContractType, String> {
    match s.to_lowercase().as_str() {
        "http" => Ok(ContractType::Http),
        "grpc" => Ok(ContractType::Grpc),
        "thrift" => Ok(ContractType::Thrift),
        "topic" => Ok(ContractType::Topic),
        "lib" => Ok(ContractType::Lib),
        "include" => Ok(ContractType::Include),
        "custom" => Ok(ContractType::Custom),
        other => Err(format!("unknown type: {other}")),
    }
}

struct ContractRecord {
    repo: String,
    contract_id: String,
    contract_type: ContractType,
    role: ContractRole,
    symbol: String,
    file: String,
    confidence: f32,
    matched: bool,
}

pub fn run(args: ContractsArgs) -> Result<(), GnxError> {
    let home_gnx = resolve_home_gnx();
    let group_dir = storage::group_dir(&home_gnx, &args.name);

    let reg = storage::read_contracts(&group_dir)?;

    // Build a set of matched symbol_uids from cross_links.
    let matched_uids: HashSet<&str> = reg
        .cross_links
        .iter()
        .flat_map(|link| vec![link.from.symbol_uid.as_str(), link.to.symbol_uid.as_str()])
        .collect();

    // Filter contracts.
    let records: Vec<ContractRecord> = reg
        .contracts
        .iter()
        .filter_map(|sc| {
            // Apply --unmatched filter.
            let is_matched = matched_uids.contains(sc.inner.symbol_uid.as_str());
            if args.unmatched && is_matched {
                return None;
            }

            // Apply --type filter.
            if let Some(ref filter_type) = args.r#type {
                if &sc.inner.contract_type != filter_type {
                    return None;
                }
            }

            // Apply --repo filter.
            if let Some(ref filter_repo) = args.repo {
                if &sc.repo != filter_repo {
                    return None;
                }
            }

            Some(ContractRecord {
                repo: sc.repo.clone(),
                contract_id: sc.inner.contract_id.clone(),
                contract_type: sc.inner.contract_type.clone(),
                role: sc.inner.role.clone(),
                symbol: sc.inner.symbol_ref.name.clone(),
                file: sc.inner.symbol_ref.file_path.clone(),
                confidence: sc.inner.confidence,
                matched: is_matched,
            })
        })
        .collect();

    if args.json {
        emit_json(&args.name, &records);
    } else {
        emit_text(&args.name, &records);
    }

    Ok(())
}

fn emit_text(name: &str, records: &[ContractRecord]) {
    println!("contracts {} ({})", name, records.len());
    for r in records {
        let role_str = match r.role {
            ContractRole::Provider => "Provider",
            ContractRole::Consumer => "Consumer",
        };
        println!(
            "  [{}] {}  ({})  {}",
            role_str, r.contract_id, r.repo, r.symbol
        );
    }
}

fn emit_json(name: &str, records: &[ContractRecord]) {
    let contracts_arr: Vec<_> = records
        .iter()
        .map(|r| {
            let contract_type_str = match r.contract_type {
                ContractType::Http => "http",
                ContractType::Grpc => "grpc",
                ContractType::Thrift => "thrift",
                ContractType::Topic => "topic",
                ContractType::Lib => "lib",
                ContractType::Include => "include",
                ContractType::Custom => "custom",
            };
            let role_str = match r.role {
                ContractRole::Provider => "provider",
                ContractRole::Consumer => "consumer",
            };
            json!({
                "repo": r.repo,
                "contract_id": r.contract_id,
                "contract_type": contract_type_str,
                "role": role_str,
                "symbol": r.symbol,
                "file": r.file,
                "confidence": r.confidence,
                "matched": r.matched,
            })
        })
        .collect();

    let out = json!({ "group": name, "contracts": contracts_arr });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()));
}
