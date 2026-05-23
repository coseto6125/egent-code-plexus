//! `bindings` section: compare per-binding resolver decisions across two
//! commits. Each binding is keyed by `(src_file, symbol_name)`.

use ecp_core::EcpError;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One resolver decision as serialized by `write_resolver_dump` in the
/// analyzer. Mirrors `DumpLine` (JSONL, one record per line).
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct BindingDecision {
    pub src_file: String,
    pub name: String,
    #[serde(default)]
    pub specifier: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub target_file: Option<String>,
    #[serde(default)]
    pub alt_count: u32,
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Debug, Serialize, Default)]
pub struct BindingsDiff {
    pub new_resolutions: Vec<BindingChange>,
    pub tier_changes: Vec<BindingChange>,
    pub target_changes: Vec<BindingChange>,
    pub removed: Vec<BindingChange>,
}

#[derive(Debug, Serialize, Clone)]
pub struct BindingChange {
    pub src_file: String,
    pub name: String,
    pub before: Option<BindingDecision>,
    pub after: Option<BindingDecision>,
}

/// Invoke `ecp admin index --repo <repo_dir> --dump-resolver <out_path>`.
pub fn dump(repo_dir: &Path, out_path: &Path) -> Result<(), EcpError> {
    let repo_str = repo_dir.to_str().ok_or_else(|| {
        EcpError::Output(format!(
            "repo path contains non-UTF-8: {}",
            repo_dir.display()
        ))
    })?;
    let out_str = out_path.to_str().ok_or_else(|| {
        EcpError::Output(format!(
            "out path contains non-UTF-8: {}",
            out_path.display()
        ))
    })?;
    crate::subprocess::run_self(&[
        "admin",
        "index",
        "--repo",
        repo_str,
        "--dump-resolver",
        out_str,
    ])?;
    Ok(())
}

/// Parse a JSONL file of `BindingDecision` records into a keyed map.
pub fn load_jsonl(path: &Path) -> Result<FxHashMap<(String, String), BindingDecision>, EcpError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| EcpError::Output(format!("read {}: {e}", path.display())))?;
    let mut map = FxHashMap::default();
    for (idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let dec: BindingDecision = serde_json::from_str(line)
            .map_err(|e| EcpError::Output(format!("JSONL line {} parse: {e}", idx + 1)))?;
        map.insert((dec.src_file.clone(), dec.name.clone()), dec);
    }
    Ok(map)
}

/// Bucket changes between baseline and current resolver decision maps.
pub fn diff(
    baseline: &FxHashMap<(String, String), BindingDecision>,
    current: &FxHashMap<(String, String), BindingDecision>,
) -> BindingsDiff {
    let mut out = BindingsDiff::default();
    let mut keys: Vec<&(String, String)> = baseline.keys().chain(current.keys()).collect();
    keys.sort();
    keys.dedup();

    for key in keys {
        let b = baseline.get(key);
        let c = current.get(key);
        let change = BindingChange {
            src_file: key.0.clone(),
            name: key.1.clone(),
            before: b.cloned(),
            after: c.cloned(),
        };
        match (b, c) {
            (None, Some(_)) => out.new_resolutions.push(change),
            (Some(_), None) => out.removed.push(change),
            (Some(bb), Some(cc)) => {
                if bb.target_file != cc.target_file {
                    out.target_changes.push(change);
                } else if bb.tier != cc.tier {
                    out.tier_changes.push(change);
                }
            }
            (None, None) => {}
        }
    }
    out
}
