use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warn,
    Info,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Impact,
    Egress,
    ShapeCheck,
    BlindSpot,
    Resolver,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Source-file attribution. Kept as a plain field for grouping in
    /// `Report::emit` (the output schema groups findings under `path`),
    /// but skipped from per-finding serialization so the row doesn't
    /// duplicate its enclosing `path`.
    #[serde(skip)]
    pub file: String,
    pub line: u32,
    pub kind: &'static str,
    pub severity: Severity,
    pub message: String,
    pub source: Source,
}

#[derive(Default, Debug)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub files_reviewed: usize,
    /// Constituents the aggregator skipped (e.g. needs cross-file context or
    /// `--baseline` not threaded through). Surfaced in `summary.deferred` so
    /// the caller knows the report is incomplete by design, not by bug.
    pub deferred: Vec<&'static str>,
}

impl Report {
    pub fn emit(&self, elapsed: std::time::Duration) -> serde_json::Value {
        let elapsed_ms = elapsed.as_millis() as u64;
        if self.findings.is_empty() {
            return serde_json::json!({
                "status": "clean",
                "files_reviewed": self.files_reviewed,
                "deferred": self.deferred,
                "elapsed_ms": elapsed_ms,
            });
        }

        let mut per_file: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
        let mut warn_count = 0usize;
        let mut info_count = 0usize;
        for f in &self.findings {
            per_file.entry(f.file.as_str()).or_default().push(f);
            match f.severity {
                Severity::Warn => warn_count += 1,
                Severity::Info => info_count += 1,
            }
        }
        let clean_files = self.files_reviewed.saturating_sub(per_file.len());

        let files: Vec<serde_json::Value> = per_file
            .into_iter()
            .map(|(path, items)| {
                let rows: Vec<serde_json::Value> = items
                    .iter()
                    .map(|f| serde_json::to_value(f).unwrap())
                    .collect();
                serde_json::json!({ "path": path, "findings": rows })
            })
            .collect();

        serde_json::json!({
            "files": files,
            "summary": {
                "files_reviewed": self.files_reviewed,
                "warn_count": warn_count,
                "info_count": info_count,
                "clean_files": clean_files,
                "deferred": self.deferred,
                "elapsed_ms": elapsed_ms,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_with_no_findings_emits_clean_status() {
        let r = Report {
            findings: vec![],
            files_reviewed: 3,
            deferred: vec![],
        };
        let v = r.emit(std::time::Duration::from_millis(42));
        assert_eq!(v["status"], "clean");
        assert_eq!(v["files_reviewed"], 3);
        assert_eq!(v["elapsed_ms"], 42);
    }

    #[test]
    fn report_groups_findings_by_file() {
        let r = Report {
            findings: vec![
                Finding {
                    file: "a.rs".into(),
                    line: 1,
                    kind: "impact",
                    severity: Severity::Info,
                    message: "8 callers".into(),
                    source: Source::Impact,
                },
                Finding {
                    file: "a.rs".into(),
                    line: 2,
                    kind: "egress",
                    severity: Severity::Warn,
                    message: "new HTTP call".into(),
                    source: Source::Egress,
                },
                Finding {
                    file: "b.rs".into(),
                    line: 5,
                    kind: "blind_spot",
                    severity: Severity::Info,
                    message: "framework x not in graph".into(),
                    source: Source::BlindSpot,
                },
            ],
            files_reviewed: 2,
            deferred: vec!["egress_diff", "shape_check", "resolver_diff"],
        };
        let v = r.emit(std::time::Duration::from_millis(10));
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(v["summary"]["warn_count"], 1);
        assert_eq!(v["summary"]["info_count"], 2);
        assert_eq!(v["summary"]["clean_files"], 0);
        assert_eq!(v["summary"]["deferred"][0], "egress_diff");
    }
}
