//! Per-file constituent dispatch and Finding collection.
//!
//! Each helper is a pure function over a `serde_json::Value` payload so it
//! can be unit-tested without a real graph or engine.

use super::findings::{Finding, Report, Severity, Source};
use crate::commands::impact::{self, Direction, ImpactArgs};
use crate::commands::tool_map::{self, ToolMapArgs};
use crate::engine::Engine;
use graph_nexus_core::GnxError;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn run(files: &[PathBuf], repo_dir: &Path, engine: &Engine) -> Result<Report, GnxError> {
    let deferred = vec!["egress_diff", "shape_check", "resolver_diff"];

    if files.is_empty() {
        return Ok(Report {
            findings: vec![],
            files_reviewed: 0,
            deferred,
        });
    }

    let file_scope: HashSet<&Path> = files.iter().map(|p| p.as_path()).collect();
    let mut findings: Vec<Finding> = Vec::new();

    findings.extend(run_impact(&file_scope, repo_dir, engine));
    findings.extend(run_coverage(&file_scope, engine));
    findings.extend(run_tool_map(&file_scope, engine));

    Ok(Report {
        findings,
        files_reviewed: files.len(),
        deferred,
    })
}

// ── impact helper ────────────────────────────────────────────────────────────

fn run_impact(file_scope: &HashSet<&Path>, repo_dir: &Path, engine: &Engine) -> Vec<Finding> {
    let args = ImpactArgs {
        name: None,
        target: None,
        baseline: Some("HEAD~1".into()),
        file: None,
        kind: None,
        direction: Direction::Up,
        depth: 3,
        high_trust_only: false,
        min_confidence: None,
        include_tests: false,
        relation_types: None,
        repo: Some(repo_dir.to_string_lossy().into_owned()),
        format: None,
    };
    let v = match impact::build_payload(&args, engine) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let scope_strs: HashSet<String> = file_scope
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    impact_findings(&v, &scope_strs)
}

/// Extract impact findings, attributing each to the changed symbol's own
/// filePath (not the requested review scope). Symbols whose filePath isn't
/// in `file_scope` are skipped. Caller-count >= 4 → `medium` risk → info.
/// Line numbers come from `changed_symbols[]` (joined on name+filePath)
/// because `impact_by_symbol[]` does not carry line.
pub fn impact_findings(v: &Value, file_scope: &HashSet<String>) -> Vec<Finding> {
    let by_sym = match v.get("impact_by_symbol").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return vec![],
    };
    let line_lookup: std::collections::HashMap<(String, String), u32> = v
        .get("changed_symbols")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s["name"].as_str()?.to_string();
                    let file = s["filePath"].as_str()?.to_string();
                    let line = s["line"].as_u64()? as u32;
                    Some(((name, file), line))
                })
                .collect()
        })
        .unwrap_or_default();
    let in_scope = |path: &str| -> bool {
        file_scope
            .iter()
            .any(|s| s == path || path.ends_with(s.as_str()) || s.ends_with(path))
    };

    let mut findings = Vec::new();
    for entry in by_sym {
        let file_path = entry["filePath"].as_str().unwrap_or("");
        if !in_scope(file_path) {
            continue;
        }
        let sym = entry["symbol"].as_str().unwrap_or("?");
        let callers = match entry["impact"].as_array() {
            Some(c) => c,
            None => continue,
        };
        let caller_count = callers
            .iter()
            .filter(|e| e["depth"].as_u64().unwrap_or(0) > 0)
            .count();
        if caller_count < 4 {
            continue;
        }
        let line = line_lookup
            .get(&(sym.to_string(), file_path.to_string()))
            .copied()
            .unwrap_or(0);
        findings.push(Finding {
            file: file_path.into(),
            line,
            kind: "impact",
            severity: Severity::Info,
            message: format!("{sym} has {caller_count} callers — review blast radius"),
            source: Source::Impact,
        });
    }
    findings
}

// ── coverage (BlindSpot) helper ──────────────────────────────────────────────

fn run_coverage(file_scope: &HashSet<&Path>, engine: &Engine) -> Vec<Finding> {
    // coverage::build_payload with --repo needs a path arg, but for blind-spot
    // extraction we need to read the graph's blind_spots directly.
    // Use the engine's graph to avoid a subprocess round-trip.
    let graph = match engine.graph() {
        Ok(g) => g,
        Err(_) => return vec![],
    };

    graph
        .blind_spots
        .iter()
        .filter_map(|bs| {
            let file_path = bs.file_path.resolve(&graph.string_pool);
            let in_scope = file_scope.iter().any(|p| {
                p.to_string_lossy() == file_path || file_path.ends_with(&*p.to_string_lossy())
            });
            if !in_scope {
                return None;
            }
            let kind = bs.kind.resolve(&graph.string_pool);
            Some(Finding {
                file: file_path.to_string(),
                line: bs.start_row.into(),
                kind: "blind_spot",
                severity: Severity::Info,
                message: format!("blind spot: {kind}"),
                source: Source::BlindSpot,
            })
        })
        .collect()
}

/// Extract coverage BlindSpot findings from a `coverage::build_payload` Value.
/// Used in unit tests — production path uses `run_coverage` (graph direct).
pub fn coverage_blind_spots(v: &Value, file_scope: &[&str]) -> Vec<Finding> {
    let scope_set: HashSet<&str> = file_scope.iter().copied().collect();
    let mut findings = Vec::new();

    // coverage payload shape: {"coverage": {"per_repo": [{"blind_spots": ...}]}}
    // or {"coverage": {"indexed_repos": ...}} — mine per_repo if present.
    if let Some(per_repo) = v.pointer("/coverage/per_repo").and_then(|v| v.as_array()) {
        for repo in per_repo {
            if let Some(by_kind) = repo
                .pointer("/blind_spots/by_kind")
                .and_then(|v| v.as_object())
            {
                for (kind, _count) in by_kind {
                    // No file info in the aggregated by_kind — emit one finding
                    // per kind for any file in scope.
                    for file in file_scope {
                        if scope_set.contains(file) {
                            findings.push(Finding {
                                file: (*file).into(),
                                line: 0,
                                kind: "blind_spot",
                                severity: Severity::Info,
                                message: format!("blind spot: {kind}"),
                                source: Source::BlindSpot,
                            });
                        }
                    }
                }
            }
        }
    }
    findings
}

// ── tool_map (egress) helper ─────────────────────────────────────────────────

fn run_tool_map(file_scope: &HashSet<&Path>, engine: &Engine) -> Vec<Finding> {
    let args = ToolMapArgs {
        category: None,
        repo: None,
        format: None,
    };
    let v = match tool_map::build_payload(&args, engine) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let file_strs: HashSet<String> = file_scope
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    tool_map_findings(&v, &file_strs)
}

/// Extract tool_map findings for call-sites in the given files.
pub fn tool_map_findings(v: &Value, file_strs: &HashSet<String>) -> Vec<Finding> {
    let mut findings = Vec::new();

    let calls = match v.get("calls").and_then(|c| c.as_object()) {
        Some(c) => c,
        None => return findings,
    };

    for (_category, entries) in calls {
        let entries = match entries.as_array() {
            Some(a) => a,
            None => continue,
        };
        for entry in entries {
            let file_path = entry["filePath"].as_str().unwrap_or("");
            if !file_strs
                .iter()
                .any(|f| f == file_path || file_path.ends_with(f.as_str()))
            {
                continue;
            }
            let callee = entry["callee"].as_str().unwrap_or("?");
            let package = entry["package"].as_str().unwrap_or("?");
            let line = entry["line"].as_u64().unwrap_or(0) as u32;
            findings.push(Finding {
                file: file_path.into(),
                line,
                kind: "egress",
                severity: Severity::Info,
                message: format!("external call: {callee} (package: {package})"),
                source: Source::Egress,
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn scope_one(path: &str) -> HashSet<String> {
        let mut s = HashSet::new();
        s.insert(path.to_string());
        s
    }

    #[test]
    fn impact_findings_baseline_mode_below_threshold_emits_nothing() {
        let v = json!({
            "status": "success",
            "baseline": "HEAD~1",
            "changed_symbols": [],
            "impact_by_symbol": [
                {
                    "symbol": "foo",
                    "filePath": "src/foo.rs",
                    "line": 12,
                    "impact": [
                        {"depth": 0, "name": "foo"},
                        {"depth": 1, "name": "a"},
                        {"depth": 1, "name": "b"},
                        {"depth": 1, "name": "c"}
                    ]
                }
            ]
        });
        let findings = impact_findings(&v, &scope_one("src/foo.rs"));
        assert!(findings.is_empty(), "expected no findings for 3 callers");
    }

    #[test]
    fn impact_findings_baseline_mode_at_threshold_emits_finding_with_correct_file() {
        let v = json!({
            "status": "success",
            "baseline": "HEAD~1",
            "changed_symbols": [
                {"name": "bar", "filePath": "src/bar.rs", "line": 42, "kind": "Function", "change_type": "modified"}
            ],
            "impact_by_symbol": [
                {
                    "symbol": "bar",
                    "filePath": "src/bar.rs",
                    "impact": [
                        {"depth": 0, "name": "bar"},
                        {"depth": 1, "name": "a"},
                        {"depth": 1, "name": "b"},
                        {"depth": 1, "name": "c"},
                        {"depth": 1, "name": "d"}
                    ]
                }
            ]
        });
        let findings = impact_findings(&v, &scope_one("src/bar.rs"));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/bar.rs");
        assert_eq!(findings[0].line, 42);
        assert_eq!(findings[0].source, Source::Impact);
        assert!(findings[0].message.contains("4 callers"));
    }

    #[test]
    fn impact_findings_skips_symbols_outside_file_scope() {
        let v = json!({
            "impact_by_symbol": [
                {
                    "symbol": "outside",
                    "filePath": "src/other.rs",
                    "line": 1,
                    "impact": [
                        {"depth": 0, "name": "outside"},
                        {"depth": 1, "name": "a"},
                        {"depth": 1, "name": "b"},
                        {"depth": 1, "name": "c"},
                        {"depth": 1, "name": "d"}
                    ]
                }
            ]
        });
        let findings = impact_findings(&v, &scope_one("src/in_scope.rs"));
        assert!(findings.is_empty(), "out-of-scope symbol must not appear");
    }

    #[test]
    fn coverage_blind_spots_maps_per_repo_findings() {
        let v = json!({
            "coverage": {
                "per_repo": [
                    {
                        "repo": "myrepo",
                        "blind_spots": {
                            "total": 1,
                            "by_kind": {
                                "dynamic-import": 1
                            }
                        }
                    }
                ]
            }
        });
        let findings = coverage_blind_spots(&v, &["src/foo.py"]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source, Source::BlindSpot);
        assert!(findings[0].message.contains("dynamic-import"));
    }

    #[test]
    fn tool_map_findings_filters_to_scope_files() {
        let v = json!({
            "status": "success",
            "totals": {"http": 2},
            "calls": {
                "http": [
                    {"callee": "axios.get", "package": "axios", "filePath": "src/api.ts", "line": 10, "col": 5},
                    {"callee": "axios.post", "package": "axios", "filePath": "src/other.ts", "line": 20, "col": 3}
                ]
            }
        });
        let findings = tool_map_findings(&v, &scope_one("src/api.ts"));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/api.ts");
        assert_eq!(findings[0].line, 10);
        assert_eq!(findings[0].source, Source::Egress);
    }

    #[test]
    fn tool_map_findings_empty_calls_yields_no_findings() {
        let v = json!({
            "status": "success",
            "totals": {},
            "calls": {}
        });
        let findings = tool_map_findings(&v, &scope_one("src/any.ts"));
        assert!(findings.is_empty());
    }
}
