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
    pub status: String,
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
    #[serde(rename = "filePath")]
    pub file_path: String,
    /// Raw BFS entries (`depth`, `name`, `kind`, `filePath`, confidence,
    /// ...). Left as `Value` on purpose: `run_bfs` returns `Vec<Value>`, so
    /// typing this field means typing its return too, and that reaches into
    /// every impact mode. Deliberate boundary, not an oversight.
    pub impact: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heuristic_callers: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downstream_callees: Option<Vec<Value>>,
}
