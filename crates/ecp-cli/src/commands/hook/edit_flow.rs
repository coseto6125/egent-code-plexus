//! Claude Code Edit/Write enrichment. Other hosts must provide their own lifecycle adapter.
use super::common::{ecp_state_dir_ensure, HookInput};
use crate::commands::{flow::load_sources, review::flow::compare_phase};
use serde_json::{json, Value};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

const MAX_CONTEXT: usize = 6000;

fn edit(input: &HookInput) -> Option<(&str, Option<&str>, &str)> {
    let path = input.tool_input.get("file_path")?.as_str()?;
    match input.tool_name.as_str() {
        "Edit" => Some((
            path,
            Some(input.tool_input.get("old_string")?.as_str()?),
            input.tool_input.get("new_string")?.as_str()?,
        )),
        "Write" => Some((path, None, input.tool_input.get("content")?.as_str()?)),
        _ => None,
    }
}

fn key(input: &HookInput) -> String {
    let mut hash = DefaultHasher::new();
    input.session_id.hash(&mut hash);
    input.tool_use_id.hash(&mut hash);
    input.tool_name.hash(&mut hash);
    input.tool_input.to_string().hash(&mut hash);
    format!("{:016x}", hash.finish())
}

pub fn context(input: &HookInput, after: bool) -> Option<String> {
    let (file, old_string, replacement) = edit(input)?;
    if after && failed(&input.tool_output) {
        if let Some(state) = super::common::ecp_state_dir(&input.cwd) {
            let _ = std::fs::remove_file(state.join(format!("flow-edit-{}.json", key(input))));
        }
        return None;
    }
    let repo = dunce::canonicalize(&input.cwd).ok()?;
    let relative = crate::commands::flow::relative_path(&repo, Path::new(file)).ok()?;
    if !crate::commands::flow::supported_path(&relative) {
        return None;
    }
    let mut current = match load_sources(&repo, None) {
        Ok(sources) => sources,
        Err(error) => return Some(format!("ecp flow: source snapshot unresolved: {error}")),
    };
    let state = ecp_state_dir_ensure(&input.cwd)?;
    let pending = state.join(format!("flow-edit-{}.json", key(input)));
    let original;
    let proposed;
    if after {
        let saved: Value = match std::fs::read(&pending).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
            Some(saved) => saved,
            None => return Some("ecp flow: before-edit snapshot unavailable; run ecp review --include flow to inspect changed consumers.".into()),
        };
        original = saved["source"].as_str()?.to_owned();
        proposed = current
            .iter()
            .find(|s| s.path == relative)
            .map(|s| s.source.clone())
            .unwrap_or_default();
        let _ = std::fs::remove_file(&pending);
        if saved["expected"].as_str() != Some(proposed.as_str()) {
            return Some("ecp flow: current source differs from the captured edit; after-edit consumers unresolved. Run ecp review --include flow.".into());
        }
    } else {
        original = current
            .iter()
            .find(|s| s.path == relative)
            .map(|s| s.source.clone())
            .unwrap_or_default();
        proposed = match old_string {
            Some(old) if !old.is_empty() && original.contains(old) => {
                if input.tool_input["replace_all"].as_bool().unwrap_or(false) {
                    original.replace(old, replacement)
                } else if original.matches(old).count() == 1 {
                    original.replacen(old, replacement, 1)
                } else {
                    return Some("ecp flow: ambiguous Edit match; consumers unresolved.".into());
                }
            }
            Some(_) => {
                return Some("ecp flow: Edit source does not match; consumers unresolved.".into())
            }
            None => replacement.to_owned(),
        };
        std::fs::write(
            &pending,
            json!({"source":original,"expected":proposed}).to_string(),
        )
        .ok()?;
    }
    let mut before = current.clone();
    replace(&mut before, &relative, original);
    replace(&mut current, &relative, proposed);
    let phase = if after { "after" } else { "before" };
    let report = compare_phase(
        &before,
        &current,
        Some(std::slice::from_ref(&relative)),
        Some(phase),
    );
    let mut rendered = render(&report, phase);
    if input.session_id.is_empty() || input.tool_use_id.is_empty() {
        rendered.push_str("Hook identity unavailable: edit pairing uses input identity; context deduplication is disabled.\n");
        return Some(rendered);
    }
    let session_hash = ecp_core::uid::xxh3_64_bytes(input.session_id.as_bytes());
    let marker = state.join(format!("flow-last-context-{session_hash:016x}"));
    // Hash includes source hashes, phase, and requested sites through the complete result.
    let fingerprint = format!(
        "{phase}:{:016x}",
        ecp_core::uid::xxh3_64_bytes(report.to_string().as_bytes())
    );
    if std::fs::read_to_string(&marker).ok().as_deref() == Some(&fingerprint) {
        return None;
    }
    std::fs::write(marker, fingerprint).ok()?;
    Some(rendered)
}

fn replace(sources: &mut Vec<ecp_analyzer::flow::SourceFile>, path: &str, source: String) {
    if let Some(file) = sources.iter_mut().find(|s| s.path == path) {
        file.source = source;
    } else {
        sources.push(ecp_analyzer::flow::SourceFile {
            path: path.into(),
            source,
        });
    }
}

fn failed(output: &Value) -> bool {
    output["is_error"].as_bool() == Some(true)
        || output["success"].as_bool() == Some(false)
        || output["exit_code"].as_i64().is_some_and(|code| code != 0)
        || output.get("error").is_some_and(|error| !error.is_null())
}

fn render(report: &Value, phase: &str) -> String {
    let mut out = format!("ecp flow {phase} edit: consumers require compatibility review. Unknown results do not establish absence.\n");
    let mut truncated = report["truncated"].as_bool().unwrap_or(false);
    if let Some(rows) = report["analysis"].as_array() {
        for row in rows {
            let analysis = &row["result"];
            let mut block = format!("{}:{} ", row["file"].as_str().unwrap_or("?"), row["line"]);
            if let Some(error) = analysis["unresolved"].as_str() {
                block.push_str(&format!("unresolved: {error}\n"));
            } else {
                let flow = &analysis["report"];
                block.push_str(&format!(
                    "source_hashes={}\n",
                    flow["source_hashes"][row["file"].as_str().unwrap_or("")]
                ));
                if let (Some(consumers), Some(nodes)) =
                    (flow["consumers"].as_array(), flow["nodes"].as_array())
                {
                    let nodes: std::collections::BTreeMap<_, _> = nodes
                        .iter()
                        .filter_map(|node| node["id"].as_u64().map(|id| (id, node)))
                        .collect();
                    for id in consumers {
                        if let Some(node) = id.as_u64().and_then(|id| nodes.get(&id)) {
                            block.push_str(&format!(
                                "  {}:{} {} {}\n",
                                node["file"].as_str().unwrap_or("?"),
                                node["line"],
                                node["kind"].as_str().unwrap_or("?"),
                                node["label"].as_str().unwrap_or("?")
                            ));
                        }
                    }
                }
                block.push_str(&format!("  boundaries={}\n", flow["boundaries"]));
                truncated |= flow["truncated"].as_bool().unwrap_or(false);
            }
            if out.len() + block.len() > MAX_CONTEXT - 300 {
                let mut end = (MAX_CONTEXT - 300).saturating_sub(out.len());
                while !block.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&block[..end]);
                out.push('\n');
                truncated = true;
                break;
            }
            out.push_str(&block);
        }
    }
    if truncated {
        out.push_str("truncated=true; run ecp review --include flow --format json for full review within analyzer budgets.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_context_edit_captures_before_and_after_consumers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.js"), "let x = 1 + 2;\nconsume(x);\n").unwrap();
        let mut input = HookInput {
            session_id: "test-session".into(),
            tool_use_id: "edit-1".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            tool_name: "Edit".into(),
            tool_input: json!({"file_path":dir.path().join("x.js"),"old_string":"1 + 2","new_string":"1 * 2"}),
            tool_output: Value::Null,
        };
        let before = context(&input, false).unwrap();
        assert!(before.contains("before edit"));
        std::fs::write(dir.path().join("x.js"), "let x = 1 * 2;\nconsume(x);\n").unwrap();
        input.tool_output = json!({"success":true});
        let after = context(&input, true).unwrap();
        assert!(after.contains("after edit"));
        assert!(context(&input, true)
            .unwrap()
            .contains("snapshot unavailable"));
    }
    #[test]
    fn test_context_session_identity_keeps_independent_evidence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.js"), "let x = 1;\nconsume(x);\n").unwrap();
        let mut input: HookInput = serde_json::from_value(json!({"session_id":"first","tool_use_id":"edit","cwd":dir.path(),"tool_name":"Edit","tool_input":{"file_path":dir.path().join("x.js"),"old_string":"1","new_string":"2"}})).unwrap();
        assert!(context(&input, false).is_some());
        assert!(context(&input, false).is_none());
        let first = key(&input);
        input.session_id = "second".into();
        assert!(context(&input, false).is_some());
        let second = key(&input);
        input.tool_output = json!({"is_error":true});
        assert!(context(&input, true).is_none());
        assert!(!dir
            .path()
            .join(format!(".ecp/flow-edit-{second}.json"))
            .exists());
        assert!(dir
            .path()
            .join(format!(".ecp/flow-edit-{first}.json"))
            .exists());
    }

    #[test]
    fn test_context_unexpected_current_source_reports_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.js");
        std::fs::write(&file, "let x = 1;\nconsume(x);\n").unwrap();
        let input: HookInput = serde_json::from_value(json!({"session_id":"first","tool_use_id":"edit","cwd":dir.path(),"tool_name":"Edit","tool_input":{"file_path":file,"old_string":"1","new_string":"2"}})).unwrap();
        assert!(context(&input, false).is_some());
        std::fs::write(&file, "let x = 3;\nconsume(x);\n").unwrap();
        assert!(context(&input, true)
            .unwrap()
            .contains("after-edit consumers unresolved"));
    }

    #[test]
    fn test_failed_error_payload_prevents_after_analysis() {
        for value in [
            json!({"is_error":true}),
            json!({"success":false}),
            json!({"exit_code":1}),
            json!({"error":"failed"}),
        ] {
            assert!(failed(&value));
        }
        assert!(!failed(&json!({"success":true})));
    }
    #[test]
    fn test_render_large_report_marks_truncation() {
        let report =
            json!({"analysis":[{"file":"x.js","line":1,"result":{"unresolved":"x".repeat(7000)}}]});
        let result = render(&report, "before");
        assert!(result.len() <= MAX_CONTEXT);
        assert!(result.contains("truncated=true"));
    }
}
