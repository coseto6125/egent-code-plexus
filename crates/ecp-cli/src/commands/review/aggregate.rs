//! Per-file constituent dispatch and Finding collection.
//!
//! Each helper is a pure function over a `serde_json::Value` payload so it
//! can be unit-tested without a real graph or engine.

use super::findings::{Finding, Report, Severity, Source};
use crate::commands::diff::{self, bindings::BindingsDiff, DiffArgs, DiffSection};
use crate::commands::impact::{self, Direction, ImpactArgs};
use crate::commands::shape_check::{self, ShapeCheckArgs};
use crate::commands::tool_map::{self, ToolMapArgs};
use crate::engine::Engine;
use crate::git::diff_parser::parse_diff_hunks;
use crate::git::safe_exec;
use ecp_core::EcpError;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub fn run(
    files: &[PathBuf],
    repo_dir: &Path,
    engine: &Engine,
    since: Option<&str>,
) -> Result<Report, EcpError> {
    let mut deferred: Vec<&'static str> = Vec::new();

    if files.is_empty() {
        deferred.extend(["egress_diff", "shape_check", "resolver_diff"]);
        return Ok(Report {
            findings: vec![],
            files_reviewed: 0,
            deferred,
        });
    }

    let scope_strs: HashSet<String> = files
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut findings: Vec<Finding> = Vec::new();

    findings.extend(run_impact(&scope_strs, repo_dir, engine, since));
    findings.extend(run_summary(&scope_strs, engine));
    findings.extend(run_tool_map(
        &scope_strs,
        engine,
        since,
        repo_dir,
        &mut deferred,
    ));
    findings.extend(run_shape_check(&scope_strs, engine));
    findings.extend(run_resolver_diff(
        &scope_strs,
        repo_dir,
        since,
        &mut deferred,
    ));

    Ok(Report {
        findings,
        files_reviewed: files.len(),
        deferred,
    })
}

/// True iff `path` is in `file_scope`. Accepts exact match OR scope-entry
/// being a suffix of `path`. The symmetric direction (`s.ends_with(path)`)
/// was unsound — it matched `vendor/rs/lib.rs` against scope `src/foo.rs`
/// via `"src/foo.rs".ends_with("rs")`.
fn path_in_scope(path: &str, file_scope: &HashSet<String>) -> bool {
    if path.is_empty() {
        return false;
    }
    file_scope
        .iter()
        .any(|s| s == path || path.ends_with(s.as_str()))
}

// ── impact helper ────────────────────────────────────────────────────────────

fn run_impact(
    file_scope: &HashSet<String>,
    repo_dir: &Path,
    engine: &Engine,
    since: Option<&str>,
) -> Vec<Finding> {
    let args = ImpactArgs {
        name: None,
        target: None,
        baseline: Some(since.unwrap_or("HEAD~1").to_string()),
        file: None,
        kind: None,
        direction: Direction::Up,
        depth: 3,
        high_trust_only: false,
        min_confidence: None,
        include_tests: false,
        relation_types: None,
        repo: Some(repo_dir.to_string_lossy().into_owned()),
        test_coverage: false,
        include_heuristic: false,
        confidence_threshold: crate::commands::impact::DEFAULT_CONFIDENCE_THRESHOLD,
        explain_confidence: false,
        format: None,
        literal: None,
    };
    let v = match impact::build_payload(&args, engine) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    impact_findings(&v, file_scope)
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
    let mut findings = Vec::new();
    for entry in by_sym {
        let Some(file_path) = entry["filePath"].as_str() else {
            continue;
        };
        if !path_in_scope(file_path, file_scope) {
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

// ── summary (BlindSpot) helper ───────────────────────────────────────────────

fn run_summary(file_scope: &HashSet<String>, engine: &Engine) -> Vec<Finding> {
    // summary::build_payload with --repo needs a path arg, but for blind-spot
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
            if !path_in_scope(file_path, file_scope) {
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

/// Mine the summary payload's per-repo `blind_spots.by_kind` aggregate.
/// The aggregate has no file-level granularity, so each (repo, kind) pair
/// yields ONE finding attributed to the repo's path — never fanned-out per
/// scope file (that would fabricate attribution). Production callers should
/// prefer `run_summary`, which reads `graph.blind_spots` directly and
/// preserves file paths.
///
/// `run_summary` (binary path) reads `graph.blind_spots` directly from the
/// engine, so this `&Value`-based variant has no in-crate caller and `cargo`
/// flags it as dead. Kept `pub` to mirror the library API surface that the
/// four sibling constituents expose (`impact_findings`, `tool_map_findings`,
/// `shape_check_findings`, `resolver_diff_findings`) for future consumers
/// that hold a serialized summary payload but no engine.
#[allow(dead_code)]
pub fn summary_blind_spots(v: &Value) -> Vec<Finding> {
    let mut findings = Vec::new();
    let Some(per_repo) = v.pointer("/summary/per_repo").and_then(|v| v.as_array()) else {
        return findings;
    };
    for repo in per_repo {
        let repo_name = repo
            .get("repo")
            .and_then(|v| v.as_str())
            .or_else(|| repo.get("name").and_then(|v| v.as_str()))
            .unwrap_or(".");
        let Some(by_kind) = repo
            .pointer("/blind_spots/by_kind")
            .and_then(|v| v.as_object())
        else {
            continue;
        };
        for kind in by_kind.keys() {
            findings.push(Finding {
                file: repo_name.into(),
                line: 0,
                kind: "blind_spot",
                severity: Severity::Info,
                message: format!("blind spot: {kind}"),
                source: Source::BlindSpot,
            });
        }
    }
    findings
}

// ── tool_map (egress) helper ─────────────────────────────────────────────────

fn run_tool_map(
    file_scope: &HashSet<String>,
    engine: &Engine,
    since: Option<&str>,
    repo_dir: &Path,
    deferred: &mut Vec<&'static str>,
) -> Vec<Finding> {
    let args = ToolMapArgs {
        category: None,
        repo: None,
        format: None,
    };
    let v = match tool_map::build_payload(&args, engine) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let added_lines = since.and_then(|r| diff_added_lines(repo_dir, r));
    if since.is_some() && added_lines.is_none() {
        deferred.push("egress_diff");
    }
    tool_map_findings(&v, file_scope, added_lines.as_ref())
}

/// Run `git diff -U0 <since>...HEAD` and build a per-file set of added lines.
/// Returns `None` when git fails (caller degrades gracefully).
fn diff_added_lines(repo_dir: &Path, since: &str) -> Option<HashMap<String, HashSet<u32>>> {
    let out = safe_exec::git()
        .args(["diff", "-U0", &format!("{since}...HEAD")])
        .current_dir(repo_dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let file_diffs = parse_diff_hunks(&text);
    let mut map: HashMap<String, HashSet<u32>> = HashMap::new();
    for fd in file_diffs {
        let lines = map.entry(fd.file_path).or_default();
        for hunk in fd.hunks {
            lines.extend(hunk.start_line..=hunk.end_line);
        }
    }
    Some(map)
}

/// Extract tool_map findings for call-sites in the given files.
///
/// `added_lines`: when `Some`, keep only call-sites whose line was added in
/// the diff. When `None`, surface all in-scope call-sites (no-`--since` mode).
pub fn tool_map_findings(
    v: &Value,
    file_strs: &HashSet<String>,
    added_lines: Option<&HashMap<String, HashSet<u32>>>,
) -> Vec<Finding> {
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
            let Some(file_path) = entry["filePath"].as_str() else {
                continue;
            };
            if !path_in_scope(file_path, file_strs) {
                continue;
            }
            let line = entry["line"].as_u64().unwrap_or(0) as u32;
            if let Some(al) = added_lines {
                // Direct key lookup: added_lines is keyed by the same file
                // path string git diff produces. Linear scan + ends_with would
                // both perf-cost AND falsely match vendor/x.rs against scope
                // key src/x.rs via suffix "x.rs".
                let in_added = al.get(file_path).is_some_and(|lines| lines.contains(&line));
                if !in_added {
                    continue;
                }
            }
            let callee = entry["callee"].as_str().unwrap_or("?");
            let package = entry["package"].as_str().unwrap_or("?");
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

// ── shape_check constituent ───────────────────────────────────────────────────

fn run_shape_check(scope_strs: &HashSet<String>, engine: &Engine) -> Vec<Finding> {
    let args = ShapeCheckArgs {
        repo: None,
        format: None,
        route: None,
    };
    let v = match shape_check::build_payload(&args, engine) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    shape_check_findings(&v, scope_strs)
}

/// Extract shape_check drift findings for consumers whose file is in scope.
pub fn shape_check_findings(v: &Value, file_scope: &HashSet<String>) -> Vec<Finding> {
    let drift = match v.get("drift").and_then(|d| d.as_array()) {
        Some(a) => a,
        None => return vec![],
    };

    drift
        .iter()
        .filter_map(|entry| {
            let consumer_file = entry["consumer_file"].as_str()?;
            if !path_in_scope(consumer_file, file_scope) {
                return None;
            }
            let consumer_name = entry["consumer_name"].as_str().unwrap_or("?");
            let route_name = entry["route_name"].as_str().unwrap_or("?");
            let drift_keys: Vec<&str> = entry["drift_keys"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            Some(Finding {
                file: consumer_file.into(),
                // Drift entries don't carry a line; graph lookup would need
                // consumer_uid → node → file_idx scan — deferred to a later pass.
                line: 0,
                kind: "drift",
                severity: Severity::Warn,
                message: format!(
                    "drift: {consumer_name} → {route_name} reads {drift_keys:?} not in route response"
                ),
                source: Source::ShapeCheck,
            })
        })
        .collect()
}

// ── resolver_diff constituent ────────────────────────────────────────────────

fn run_resolver_diff(
    scope_strs: &HashSet<String>,
    repo_dir: &Path,
    since: Option<&str>,
    deferred: &mut Vec<&'static str>,
) -> Vec<Finding> {
    let since = match since {
        // No baseline ref → resolver_diff stays deferred.
        None => {
            deferred.push("resolver_diff");
            return vec![];
        }
        Some(s) => s,
    };
    let args = DiffArgs {
        section: vec![DiffSection::Bindings],
        baseline: Some(since.to_string()),
        baseline_graph: None,
        current_graph: None,
        format: None,
        verbose: false,
        repo: Some(repo_dir.to_string_lossy().into_owned()),
    };
    let payload = match diff::build_payload(&args) {
        Ok(p) => p,
        Err(_) => return vec![],
    };
    let bindings = match payload.bindings.as_ref() {
        Some(b) => b,
        None => return vec![],
    };
    resolver_diff_findings(bindings, scope_strs)
}

/// Emit a warn-level finding for each tier change whose `src_file` is in scope.
///
/// All `tier_changes` are considered degradations in the review context: any
/// resolver tier shift (SameFile → Global, ImportScoped → Unresolved, etc.)
/// means the graph's confidence in that binding dropped, which is worth
/// surfacing to the LLM making the change.
pub fn resolver_diff_findings(b: &BindingsDiff, file_scope: &HashSet<String>) -> Vec<Finding> {
    b.tier_changes
        .iter()
        .filter_map(|chg| {
            if !path_in_scope(&chg.src_file, file_scope) {
                return None;
            }
            let from = chg
                .before
                .as_ref()
                .and_then(|d| d.tier.as_deref())
                .unwrap_or("?");
            let to = chg
                .after
                .as_ref()
                .and_then(|d| d.tier.as_deref())
                .unwrap_or("?");
            Some(Finding {
                file: chg.src_file.clone(),
                // BindingChange carries no line; src_file is the caller file.
                line: 0,
                kind: "resolver_drift",
                severity: Severity::Warn,
                message: format!("binding {} re-resolved: {} → {}", chg.name, from, to),
                source: Source::Resolver,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::diff::bindings::{BindingChange, BindingDecision, BindingsDiff};
    use serde_json::json;

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
    fn summary_blind_spots_maps_per_repo_findings() {
        let v = json!({
            "summary": {
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
        let findings = summary_blind_spots(&v);
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
        let findings = tool_map_findings(&v, &scope_one("src/api.ts"), None);
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
        let findings = tool_map_findings(&v, &scope_one("src/any.ts"), None);
        assert!(findings.is_empty());
    }

    #[test]
    fn tool_map_findings_diff_filter_keeps_added_lines_only() {
        let v = json!({
            "calls": {
                "http": [
                    {"callee": "fetch", "package": "node-fetch", "filePath": "src/api.ts", "line": 5},
                    {"callee": "axios.get", "package": "axios", "filePath": "src/api.ts", "line": 20}
                ]
            }
        });
        let mut added: HashMap<String, HashSet<u32>> = HashMap::new();
        added.entry("src/api.ts".into()).or_default().insert(5); // only line 5 added
        let findings = tool_map_findings(&v, &scope_one("src/api.ts"), Some(&added));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 5);
    }

    #[test]
    fn tool_map_findings_diff_filter_empty_added_set_emits_nothing() {
        let v = json!({
            "calls": {
                "http": [
                    {"callee": "fetch", "package": "node-fetch", "filePath": "src/api.ts", "line": 10}
                ]
            }
        });
        let added: HashMap<String, HashSet<u32>> = HashMap::new();
        let findings = tool_map_findings(&v, &scope_one("src/api.ts"), Some(&added));
        assert!(findings.is_empty());
    }

    // ── shape_check tests ─────────────────────────────────────────────────────

    #[test]
    fn shape_check_findings_emits_warn_for_in_scope_consumer() {
        let v = json!({
            "status": "success",
            "total_fetches": 1,
            "drift_count": 1,
            "drift": [
                {
                    "consumer_uid": "uid-1",
                    "consumer_name": "fetchUser",
                    "consumer_file": "src/api.ts",
                    "route_uid": "route-1",
                    "route_name": "GET /users/:id",
                    "drift_keys": ["email", "avatar"],
                    "response_keys": ["id", "name"],
                    "error_keys": ["error"],
                    "fetch_count": 3
                }
            ]
        });
        let findings = shape_check_findings(&v, &scope_one("src/api.ts"));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warn);
        assert_eq!(findings[0].source, Source::ShapeCheck);
        assert_eq!(findings[0].kind, "drift");
        assert_eq!(findings[0].file, "src/api.ts");
        assert_eq!(findings[0].line, 0);
        assert!(findings[0].message.contains("fetchUser"));
        assert!(findings[0].message.contains("GET /users/:id"));
    }

    #[test]
    fn shape_check_findings_skips_out_of_scope() {
        let v = json!({
            "drift": [
                {
                    "consumer_uid": "uid-2",
                    "consumer_name": "fetchOrder",
                    "consumer_file": "src/other.ts",
                    "route_uid": "route-2",
                    "route_name": "GET /orders/:id",
                    "drift_keys": ["total"],
                    "response_keys": [],
                    "error_keys": [],
                    "fetch_count": 1
                }
            ]
        });
        let findings = shape_check_findings(&v, &scope_one("src/api.ts"));
        assert!(findings.is_empty(), "out-of-scope consumer must not appear");
    }

    #[test]
    fn shape_check_findings_empty_drift_yields_no_findings() {
        let v = json!({"status": "success", "total_fetches": 5, "drift_count": 0, "drift": []});
        let findings = shape_check_findings(&v, &scope_one("src/api.ts"));
        assert!(findings.is_empty());
    }

    // ── resolver_diff tests ───────────────────────────────────────────────────

    fn make_binding_change(
        src_file: &str,
        name: &str,
        from_tier: &str,
        to_tier: &str,
    ) -> BindingChange {
        BindingChange {
            src_file: src_file.into(),
            name: name.into(),
            before: Some(BindingDecision {
                src_file: src_file.into(),
                name: name.into(),
                specifier: None,
                tier: Some(from_tier.into()),
                target_file: None,
                alt_count: 0,
                confidence: Some(0.9),
            }),
            after: Some(BindingDecision {
                src_file: src_file.into(),
                name: name.into(),
                specifier: None,
                tier: Some(to_tier.into()),
                target_file: None,
                alt_count: 2,
                confidence: Some(0.4),
            }),
        }
    }

    #[test]
    fn resolver_diff_findings_filters_by_caller_file() {
        let b = BindingsDiff {
            new_resolutions: vec![],
            tier_changes: vec![
                make_binding_change("src/in_scope.rs", "parseData", "SameFile", "Global"),
                make_binding_change("src/other.rs", "helper", "ImportScoped", "Unresolved"),
            ],
            target_changes: vec![],
            removed: vec![],
        };
        let findings = resolver_diff_findings(&b, &scope_one("src/in_scope.rs"));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/in_scope.rs");
        assert_eq!(findings[0].severity, Severity::Warn);
        assert_eq!(findings[0].source, Source::Resolver);
        assert_eq!(findings[0].kind, "resolver_drift");
        assert!(findings[0].message.contains("parseData"));
        assert!(findings[0].message.contains("SameFile"));
        assert!(findings[0].message.contains("Global"));
    }

    #[test]
    fn resolver_diff_findings_no_tier_changes_yields_nothing() {
        let b = BindingsDiff::default();
        let findings = resolver_diff_findings(&b, &scope_one("src/foo.rs"));
        assert!(findings.is_empty());
    }

    #[test]
    fn resolver_diff_findings_all_out_of_scope_yields_nothing() {
        let b = BindingsDiff {
            tier_changes: vec![make_binding_change(
                "src/other.rs",
                "fn_x",
                "SameFile",
                "Global",
            )],
            ..Default::default()
        };
        let findings = resolver_diff_findings(&b, &scope_one("src/in_scope.rs"));
        assert!(findings.is_empty());
    }
}
