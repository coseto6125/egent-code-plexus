//! Source-based change review. Both snapshots retain consumers even when topology is unchanged.
use ecp_analyzer::flow::{analyze_changes, Budgets, SourceFile};
use ecp_core::EcpError;
use serde_json::{json, Value};
use similar::{ChangeTag, TextDiff};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn build(repo: &Path, baseline: &str, files: Option<&[String]>) -> Result<Value, EcpError> {
    crate::git::safe_exec::reject_option_like_rev(baseline)?;
    let repo = dunce::canonicalize(repo)?;
    let files = files
        .map(|files| {
            files
                .iter()
                .map(|file| crate::commands::flow::relative_path(&repo, Path::new(file)))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let before = crate::commands::flow::load_sources_at_ref(&repo, baseline)?;
    let after = crate::commands::flow::load_sources(&repo, None)?;
    Ok(compare(&before, &after, files.as_deref()))
}

pub fn compare(before: &[SourceFile], after: &[SourceFile], files: Option<&[String]>) -> Value {
    compare_phase(before, after, files, None)
}

pub fn compare_phase(
    before: &[SourceFile],
    after: &[SourceFile],
    files: Option<&[String]>,
    phase: Option<&str>,
) -> Value {
    let old: BTreeMap<_, _> = before
        .iter()
        .map(|s| (s.path.as_str(), s.source.as_str()))
        .collect();
    let new: BTreeMap<_, _> = after
        .iter()
        .map(|s| (s.path.as_str(), s.source.as_str()))
        .collect();
    let paths: BTreeSet<_> = old.keys().chain(new.keys()).copied().collect();
    let mut old_changes = BTreeMap::new();
    let mut new_changes = BTreeMap::new();
    for path in paths {
        if files.is_some_and(|files| !files.iter().any(|file| file == path)) {
            continue;
        }
        let old_source = old.get(path).copied().unwrap_or("");
        let new_source = new.get(path).copied().unwrap_or("");
        if old_source == new_source {
            continue;
        }
        let (old_lines, new_lines) = changed_lines(old_source, new_source);
        if !old_lines.is_empty() {
            old_changes.insert(path.to_owned(), old_lines.into_iter().collect());
        }
        if !new_lines.is_empty() {
            new_changes.insert(path.to_owned(), new_lines.into_iter().collect());
        }
    }
    let mut reports = Vec::new();
    let mut truncated = false;
    for (snapshot, sources, changes) in [
        ("before", before, old_changes),
        ("after", after, new_changes),
    ] {
        if phase.is_some_and(|phase| phase != snapshot) || changes.is_empty() {
            continue;
        }
        let result = match analyze_changes(sources, &changes, &Budgets::default()) {
            Ok(report) => {
                truncated |= report.truncated;
                json!({"report":report})
            }
            Err(message) => json!({"unresolved":message}),
        };
        let (file, lines) = changes.first_key_value().expect("nonempty changes");
        reports.push(json!({"snapshot":snapshot,"file":if changes.len() == 1 {file.as_str()} else {"<multiple files>"},"line":lines[0],"changed_files":changes,"result":result}));
    }
    json!({"snapshots":"baseline_and_working_tree", "analysis":reports,"truncated":truncated,
        "coverage":"Changed lines in JS/TS, Python and PHP sources admitted by flow source filters. Other files remain outside coverage. Before and after consumers require compatibility review. Unresolved or truncated results do not establish absence."})
}

pub fn changed_lines(before: &str, after: &str) -> (BTreeSet<usize>, BTreeSet<usize>) {
    let mut old = BTreeSet::new();
    let mut new = BTreeSet::new();
    for change in TextDiff::from_lines(before, after).iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => {
                if let Some(line) = change.old_index() {
                    old.insert(line + 1);
                }
            }
            ChangeTag::Insert => {
                if let Some(line) = change.new_index() {
                    new.insert(line + 1);
                }
            }
            ChangeTag::Equal => {}
        }
    }
    (old, new)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_changed_lines_deletion_retains_before_anchor() {
        assert_eq!(
            changed_lines("let x = 1;\nuse(x);\n", "use(x);\n"),
            (BTreeSet::from([1]), BTreeSet::new())
        );
    }
    #[test]
    fn test_compare_deleted_definition_preserves_old_consumers() {
        let before = vec![SourceFile {
            path: "x.js".into(),
            source: "let x = 1;\nconsume(x);\n".into(),
        }];
        let after = vec![SourceFile {
            path: "x.js".into(),
            source: "consume(x);\n".into(),
        }];
        let report = compare(&before, &after, None);
        let analyses = report["analysis"].as_array().unwrap();
        assert_eq!(analyses.len(), 1);
        assert_eq!(analyses[0]["snapshot"], "before");
        assert!(!analyses[0]["result"]["report"]["consumers"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn test_compare_operation_change_keeps_both_snapshots() {
        let before = vec![SourceFile {
            path: "x.js".into(),
            source: "let x = 1 + 2;\nconsume(x);\n".into(),
        }];
        let after = vec![SourceFile {
            path: "x.js".into(),
            source: "let x = 1 * 2;\nconsume(x);\n".into(),
        }];
        let report = compare(&before, &after, None);
        let analyses = report["analysis"].as_array().unwrap();
        assert!(analyses.iter().any(|r| r["snapshot"] == "before"));
        assert!(analyses.iter().any(|r| r["snapshot"] == "after"));
        for analysis in analyses {
            let flow = &analysis["result"]["report"];
            assert!(
                flow["consumers"]
                    .as_array()
                    .is_some_and(|consumers| !consumers.is_empty()),
                "{analysis}"
            );
        }
    }
}
