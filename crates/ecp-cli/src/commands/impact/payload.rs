//! Typed envelope for `ecp impact --baseline`.
//!
//! Two consumers navigate this payload today: `review::aggregate` in-process
//! (via `build_baseline_payload`) and `dev::pr_analyze` out-of-process, over
//! a subprocess's stdout (via `serde_json::from_slice`). Both used to walk a
//! `serde_json::Value` (or a hand-synced mirror struct) by string key, so a
//! renamed field was a silent `unwrap_or("?")` at runtime instead of a
//! compile error. This module is the single source of truth for the shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level JSON envelope emitted by `ecp impact --baseline <ref>`.
///
/// `#[serde(default)]` on the `Vec` fields lets an older `ecp` binary's
/// output (missing a field added in a later release) still deserialize —
/// `dev::pr_analyze` reads this from a subprocess it does not control the
/// version of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselinePayload {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub baseline: String,
    /// Set only on the "0 changes detected" short-circuit (no `git diff`
    /// hunks between baseline and HEAD); absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub changed_symbols: Vec<ChangedSymbol>,
    #[serde(default)]
    pub impact_by_symbol: Vec<ImpactBySymbol>,
}

/// One symbol whose source body changed between baseline and HEAD.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedSymbol {
    pub name: String,
    /// `"Function"`, `"Method"`, `"Struct"`, `"Module"`, etc.
    pub kind: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub line: u32,
    pub change_type: String,
}

/// Per-changed-symbol BFS result: the symbol plus its blast radius.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactBySymbol {
    pub symbol: String,
    #[serde(rename = "filePath", default)]
    pub file_path: String,
    /// Raw BFS entries (`depth`, `name`, `kind`, `filePath`, confidence,
    /// ...). Left as `Value` on purpose: `run_bfs` returns `Vec<Value>`, so
    /// typing this field means typing its return too, and that reaches into
    /// every impact mode. Deliberate boundary, not an oversight — but a
    /// consumer computing risk from these entries must still validate them:
    /// see [`validate_impact_entries`].
    #[serde(default)]
    pub impact: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heuristic_callers: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downstream_callees: Option<Vec<Value>>,
}

/// The fields a risk calculation reads out of a BFS entry.
///
/// The envelope keeps `impact` as `Vec<Value>` so the producer does not have
/// to type `run_bfs`'s return — but a consumer must not read a malformed
/// entry as "depth 0, no name" and quietly report a smaller blast radius.
/// [`validate_impact_entries`] is how a reader gets the strictness the
/// hand-written mirror used to provide for free.
#[derive(Debug, Clone, Deserialize)]
pub struct ImpactEntry {
    pub name: String,
    /// 0 = the changed symbol itself; >0 = transitive callers.
    pub depth: u32,
}

/// Reject a payload whose BFS entries are missing `name` or `depth`.
///
/// Restores the contract the mirror struct enforced by construction: an
/// entry that does not parse is a producer/consumer version mismatch, and
/// silently under-counting the impact set would understate the risk label.
pub fn validate_impact_entries(payload: &BaselinePayload) -> Result<(), String> {
    for group in &payload.impact_by_symbol {
        for entry in &group.impact {
            serde_json::from_value::<ImpactEntry>(entry.clone())
                .map_err(|e| format!("impact entry for {:?}: {e}", group.symbol))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload_with_entry(entry: serde_json::Value) -> BaselinePayload {
        BaselinePayload {
            status: "success".into(),
            baseline: "HEAD~1".into(),
            message: None,
            changed_paths: vec![],
            changed_symbols: vec![],
            impact_by_symbol: vec![ImpactBySymbol {
                symbol: "target".into(),
                file_path: "src/x.rs".into(),
                impact: vec![entry],
                heuristic_callers: None,
                downstream_callees: None,
            }],
        }
    }

    #[test]
    fn validate_accepts_a_well_formed_entry() {
        let p = payload_with_entry(serde_json::json!({"name": "caller", "depth": 1}));
        assert!(validate_impact_entries(&p).is_ok());
    }

    #[test]
    fn validate_rejects_an_entry_missing_depth() {
        let p = payload_with_entry(serde_json::json!({"name": "caller"}));
        let err = validate_impact_entries(&p).expect_err("missing depth must not pass");
        assert!(err.contains("target"), "error names the symbol: {err}");
    }

    #[test]
    fn validate_rejects_an_entry_missing_name() {
        let p = payload_with_entry(serde_json::json!({"depth": 2}));
        assert!(validate_impact_entries(&p).is_err());
    }

    /// An older `ecp` binary omits fields a later one emits; pr-analyze reads
    /// a subprocess whose version it does not control.
    #[test]
    fn envelope_tolerates_an_older_binarys_output() {
        let legacy = r#"{"impact_by_symbol":[{"symbol":"s"}]}"#;
        let p: BaselinePayload = serde_json::from_str(legacy).expect("legacy payload must parse");
        assert_eq!(p.impact_by_symbol.len(), 1);
        assert!(p.impact_by_symbol[0].impact.is_empty());
        assert!(p.changed_symbols.is_empty());
    }
}
