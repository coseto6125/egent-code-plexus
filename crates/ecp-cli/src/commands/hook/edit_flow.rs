//! Claude Code Edit/Write enrichment: value-flow evidence for the edited
//! source. Other hosts must provide their own lifecycle adapter.
//!
//! The hook runs on every Edit/Write and blocks the tool call, so it reads
//! the edited file plus a bounded set of its direct importers taken from the
//! published graph, and never walks the repository. Deeper consumers are the
//! job of `ecp review --include flow`, and the rendered header says so.
use super::common::{lookup_index_dir, HookInput};
use crate::commands::flow::{relative_path, supported_path};
use crate::commands::graph_csr::iter_incoming_edges_filtered;
use crate::commands::review::flow::compare_phase;
use crate::engine::Engine;
use ecp_analyzer::flow::SourceFile;
use ecp_core::graph::ArchivedRelType;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_CONTEXT: usize = 6000;
/// One source file per hook call; larger files belong to the CLI paths.
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
/// Cross-file evidence stays bounded so the hook stays a hot path.
const MAX_IMPORTERS: usize = 24;
const MAX_IMPORTER_BYTES: usize = 512 * 1024;
/// A before-edit snapshot whose PostToolUse never arrived is garbage after this.
const PENDING_TTL: Duration = Duration::from_secs(60 * 60);

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
    context_in(
        input,
        after,
        &ecp_core::registry::resolve_home_ecp().join("flow-edit"),
    )
}

/// `state` carries the before-edit snapshot between the two hook processes
/// plus the last-context marker. It lives under the user's ecp home rather
/// than the repository because the snapshot holds source text.
pub fn context_in(input: &HookInput, after: bool, state: &Path) -> Option<String> {
    let (file, old_string, replacement) = edit(input)?;
    let pending = state.join(format!("{}.json", key(input)));
    if after && failed(&input.tool_output) {
        let _ = std::fs::remove_file(&pending);
        return None;
    }
    let cwd = Path::new(&input.cwd);
    // Hook paths share the host's spelling of cwd, including symlink aliases
    // and Windows verbatim prefixes. New Write targets need not exist yet.
    let relative = relative_path(cwd, Path::new(file)).ok().or_else(|| {
        let repo = dunce::canonicalize(cwd).ok()?;
        relative_path(&repo, Path::new(file)).ok()
    })?;
    if !supported_path(&relative) {
        return None;
    }
    let absolute = if Path::new(file).is_absolute() {
        PathBuf::from(file)
    } else {
        cwd.join(file)
    };
    let current = match read_source(&absolute) {
        Ok(source) => source,
        Err(reason) => {
            return Some(format!(
                "ecp flow: {relative} {reason}; consumers unresolved."
            ))
        }
    };
    ensure_private_dir(state)?;
    let (original, proposed) = if after {
        let saved: Value = match std::fs::read(&pending)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        {
            Some(saved) => saved,
            None => return Some("ecp flow: before-edit snapshot unavailable; run ecp review --include flow to inspect changed consumers.".into()),
        };
        let original = saved["source"].as_str()?.to_owned();
        let _ = std::fs::remove_file(&pending);
        if saved["expected"].as_str() != Some(current.as_str()) {
            return Some("ecp flow: current source differs from the captured edit; after-edit consumers unresolved. Run ecp review --include flow.".into());
        }
        (original, current)
    } else {
        prune_pending(state);
        let proposed = match old_string {
            Some(old) if !old.is_empty() && current.contains(old) => {
                if input.tool_input["replace_all"].as_bool().unwrap_or(false) {
                    current.replace(old, replacement)
                } else if current.matches(old).count() == 1 {
                    current.replacen(old, replacement, 1)
                } else {
                    return Some("ecp flow: ambiguous Edit match; consumers unresolved.".into());
                }
            }
            Some(_) => {
                return Some("ecp flow: Edit source does not match; consumers unresolved.".into())
            }
            None => replacement.to_owned(),
        };
        write_private(
            &pending,
            json!({"source": current, "expected": proposed}).to_string(),
        )
        .ok()?;
        (current, proposed)
    };
    let importers = importers(&input.cwd, cwd, &relative);
    let mut before = vec![SourceFile {
        path: relative.clone(),
        source: original,
    }];
    before.extend(importers.files.iter().cloned());
    let mut after_edit = vec![SourceFile {
        path: relative.clone(),
        source: proposed,
    }];
    after_edit.extend(importers.files);
    let phase = if after { "after" } else { "before" };
    let report = compare_phase(
        &before,
        &after_edit,
        Some(std::slice::from_ref(&relative)),
        Some(phase),
    );
    let scope = importers.scope;
    if input.session_id.is_empty() || input.tool_use_id.is_empty() {
        let warning = "Hook identity unavailable: edit pairing uses input identity; context deduplication is disabled.\n";
        let mut rendered = render(&report, phase, &scope, MAX_CONTEXT - warning.len());
        rendered.push_str(warning);
        return Some(rendered);
    }
    let rendered = render(&report, phase, &scope, MAX_CONTEXT);
    let session_hash = ecp_core::uid::xxh3_64_bytes(input.session_id.as_bytes());
    let marker = state.join(format!("last-{session_hash:016x}"));
    // Hash includes source hashes, phase, and requested sites through the complete result.
    let fingerprint = format!(
        "{phase}:{:016x}",
        ecp_core::uid::xxh3_64_bytes(report.to_string().as_bytes())
    );
    if std::fs::read_to_string(&marker).ok().as_deref() == Some(&fingerprint) {
        return None;
    }
    write_private(&marker, fingerprint).ok()?;
    Some(rendered)
}

struct Importers {
    files: Vec<SourceFile>,
    scope: String,
}

/// Direct importers of `relative` from the published graph: every file with
/// an `Imports` edge into a node of the edited file. The graph keys paths at
/// the worktree root, so an exact path match also proves `cwd` is that root;
/// otherwise, and without a graph, the scope is the edited file alone.
fn importers(cwd_text: &str, cwd: &Path, relative: &str) -> Importers {
    let single = Importers {
        files: Vec::new(),
        scope: "edited file only; cross-file consumers: ecp review --include flow --baseline <ref>"
            .into(),
    };
    let Some(index_dir) = lookup_index_dir(cwd_text) else {
        return single;
    };
    let Ok(engine) = Engine::load(index_dir.join("graph.bin")) else {
        return single;
    };
    let Ok(graph) = engine.graph() else {
        return single;
    };
    let Some(edited) = graph
        .files
        .iter()
        .position(|file| file.path.resolve(&graph.string_pool) == relative)
    else {
        return single;
    };
    let edited = edited as u32;
    let mut sources = BTreeSet::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        if node.file_idx.to_native() != edited {
            continue;
        }
        let imports = |rel: &ArchivedRelType| matches!(rel, ArchivedRelType::Imports);
        for (source, _) in iter_incoming_edges_filtered(graph, idx as u32, imports) {
            let importer = graph.nodes[source as usize].file_idx.to_native();
            if importer != edited {
                sources.insert(importer);
            }
        }
    }
    let mut files = Vec::new();
    let mut omitted = 0usize;
    let mut bytes = 0usize;
    for importer in sources {
        let path = graph.files[importer as usize]
            .path
            .resolve(&graph.string_pool)
            .to_owned();
        if !supported_path(&path) {
            omitted += 1;
            continue;
        }
        match read_source(&cwd.join(&path)) {
            Ok(source)
                if files.len() < MAX_IMPORTERS && bytes + source.len() <= MAX_IMPORTER_BYTES =>
            {
                bytes += source.len();
                files.push(SourceFile { path, source });
            }
            _ => omitted += 1,
        }
    }
    Importers {
        scope: format!(
            "edited file and {} direct importers from the graph ({omitted} omitted); deeper consumers: ecp review --include flow --baseline <ref>",
            files.len()
        ),
        files,
    }
}

fn read_source(path: &Path) -> Result<String, String> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(format!("is unreadable ({error})")),
        Ok(metadata) if metadata.len() > MAX_SOURCE_BYTES => {
            return Err(format!(
                "exceeds the {MAX_SOURCE_BYTES}-byte hook source budget"
            ))
        }
        Ok(_) => {}
    }
    let bytes = std::fs::read(path).map_err(|error| format!("is unreadable ({error})"))?;
    String::from_utf8(bytes).map_err(|_| "is not valid UTF-8".into())
}

fn ensure_private_dir(dir: &Path) -> Option<()> {
    std::fs::create_dir_all(dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(())
}

fn write_private(path: &Path, contents: String) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(contents.as_bytes())
}

/// Drop snapshots whose PostToolUse never came (interrupted or rejected edits).
fn prune_pending(state: &Path) {
    let Ok(entries) = std::fs::read_dir(state) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = path.extension().is_some_and(|ext| ext == "json")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > PENDING_TTL);
        if stale {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn failed(output: &Value) -> bool {
    output["is_error"].as_bool() == Some(true)
        || output["success"].as_bool() == Some(false)
        || output["exit_code"].as_i64().is_some_and(|code| code != 0)
        || output.get("error").is_some_and(|error| !error.is_null())
}

/// Source text goes into the agent's context verbatim; keep it on one line.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn render(report: &Value, phase: &str, scope: &str, max_context: usize) -> String {
    let mut lines = vec![format!(
        "ecp flow {phase} edit: consumers of the edited value require compatibility review. Unknown results do not establish absence. Scope: {scope}."
    )];
    let mut truncated = report["truncated"].as_bool().unwrap_or(false);
    for row in report["analysis"].as_array().into_iter().flatten() {
        let file = row["file"].as_str().unwrap_or("?");
        lines.push(format!("{}:{}", one_line(file), row["line"]));
        let analysis = &row["result"];
        if let Some(error) = analysis["unresolved"].as_str() {
            lines.push(format!("  unresolved: {}", one_line(error)));
            continue;
        }
        let flow = &analysis["report"];
        lines.push(format!("  source_hashes={}", flow["source_hashes"][file]));
        if let (Some(consumers), Some(nodes)) =
            (flow["consumers"].as_array(), flow["nodes"].as_array())
        {
            let nodes: std::collections::BTreeMap<_, _> = nodes
                .iter()
                .filter_map(|node| node["id"].as_u64().map(|id| (id, node)))
                .collect();
            for node in consumers
                .iter()
                .filter_map(|id| id.as_u64().and_then(|id| nodes.get(&id)))
            {
                lines.push(format!(
                    "  {}:{} {} {}",
                    one_line(node["file"].as_str().unwrap_or("?")),
                    node["line"],
                    node["kind"].as_str().unwrap_or("?"),
                    one_line(node["label"].as_str().unwrap_or("?"))
                ));
            }
        }
        let boundaries = flow["boundaries"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        lines.push(format!(
            "  boundaries={} omitted={}",
            boundaries.len(),
            flow["boundaries_omitted"]
        ));
        for boundary in boundaries.iter().take(5) {
            lines.push(format!(
                "  boundary {}:{} {}",
                one_line(boundary["file"].as_str().unwrap_or("?")),
                boundary["line"],
                boundary["kind"].as_str().unwrap_or("?")
            ));
        }
        truncated |= flow["truncated"].as_bool().unwrap_or(false);
    }
    let footer = "truncated=true; run ecp review --include flow --format json for full review within analyzer budgets.\n";
    let budget = max_context - footer.len();
    let mut out = String::new();
    for line in lines {
        if out.len() + line.len() + 1 > budget {
            // Whole lines only, except a single line that is itself over budget.
            let mut end = budget.saturating_sub(out.len() + 1).min(line.len());
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            if out.is_empty() {
                out.push_str(&line[..end]);
                out.push('\n');
            }
            truncated = true;
            break;
        }
        out.push_str(&line);
        out.push('\n');
    }
    if truncated {
        out.push_str(footer);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit_input(cwd: &Path, file: &Path, old: &str, new: &str) -> HookInput {
        serde_json::from_value(json!({
            "session_id": "session",
            "tool_use_id": "edit",
            "cwd": cwd,
            "tool_name": "Edit",
            "tool_input": {"file_path": file, "old_string": old, "new_string": new}
        }))
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn test_context_symlink_cwd_preserves_edit_consumers() {
        let temp = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let alias = temp.path().join("alias");
        std::fs::create_dir(&repo).unwrap();
        std::os::unix::fs::symlink(&repo, &alias).unwrap();
        std::fs::write(repo.join("x.js"), "let x = 1;\nconsume(x);\n").unwrap();
        let input = edit_input(&alias, &alias.join("x.js"), "1", "2");
        assert!(context_in(&input, false, state.path())
            .unwrap()
            .contains("x.js:2"));
        std::fs::write(repo.join("x.js"), "let x = 2;\nconsume(x);\n").unwrap();
        assert!(context_in(&input, true, state.path())
            .unwrap()
            .contains("x.js:2"));
    }

    /// Contract: the hook reads the edited file and nothing else. A sibling the
    /// process cannot read must not turn the evidence into "unresolved".
    #[cfg(unix)]
    #[test]
    fn test_context_reads_only_the_edited_file() {
        use std::os::unix::fs::PermissionsExt;
        let repo = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("x.js"), "let x = 1;\nconsume(x);\n").unwrap();
        let sibling = repo.path().join("y.js");
        std::fs::write(&sibling, "import { x } from './x.js';\nconsume(x);\n").unwrap();
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o000)).unwrap();
        let input = edit_input(repo.path(), &repo.path().join("x.js"), "1", "2");
        let rendered = context_in(&input, false, state.path()).unwrap();
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(rendered.contains("x.js:2"), "{rendered}");
        assert!(!rendered.contains("unresolved"), "{rendered}");
        assert!(rendered.contains("Scope: edited file only"), "{rendered}");
    }

    #[test]
    fn test_context_write_new_file_preserves_before_and_after_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let file = temp.path().join("new.js");
        let source = "let x = 1;\nconsume(x);\n";
        let input: HookInput = serde_json::from_value(json!({"session_id":"write","tool_use_id":"new","cwd":temp.path(),"tool_name":"Write","tool_input":{"file_path":file,"content":source}})).unwrap();
        assert!(context_in(&input, false, state.path())
            .unwrap()
            .contains("before edit"));
        std::fs::write(&file, source).unwrap();
        assert!(context_in(&input, true, state.path())
            .unwrap()
            .contains("new.js:2"));
    }

    #[test]
    fn test_context_edit_captures_before_and_after_consumers() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.js"), "let x = 1 + 2;\nconsume(x);\n").unwrap();
        let mut input = edit_input(dir.path(), &dir.path().join("x.js"), "1 + 2", "1 * 2");
        let before = context_in(&input, false, state.path()).unwrap();
        assert!(before.contains("before edit"));
        std::fs::write(dir.path().join("x.js"), "let x = 1 * 2;\nconsume(x);\n").unwrap();
        input.tool_output = json!({"success":true});
        let after = context_in(&input, true, state.path()).unwrap();
        assert!(after.contains("after edit"));
        assert!(context_in(&input, true, state.path())
            .unwrap()
            .contains("snapshot unavailable"));
    }

    #[test]
    fn test_context_session_identity_keeps_independent_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.js"), "let x = 1;\nconsume(x);\n").unwrap();
        let mut input = edit_input(dir.path(), &dir.path().join("x.js"), "1", "2");
        input.session_id = "first".into();
        assert!(context_in(&input, false, state.path()).is_some());
        assert!(context_in(&input, false, state.path()).is_none());
        let first = key(&input);
        input.session_id = "second".into();
        assert!(context_in(&input, false, state.path()).is_some());
        let second = key(&input);
        input.tool_output = json!({"is_error":true});
        assert!(context_in(&input, true, state.path()).is_none());
        assert!(!state.path().join(format!("{second}.json")).exists());
        assert!(state.path().join(format!("{first}.json")).exists());
        assert!(
            !dir.path().join(".ecp").exists(),
            "no state inside the repository"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_context_snapshot_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap().path().join("flow-edit");
        std::fs::write(dir.path().join("x.js"), "let x = 1;\nconsume(x);\n").unwrap();
        let input = edit_input(dir.path(), &dir.path().join("x.js"), "1", "2");
        assert!(context_in(&input, false, &state).is_some());
        let pending = state.join(format!("{}.json", key(&input)));
        assert_eq!(
            std::fs::metadata(&pending).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn test_prune_pending_drops_only_stale_snapshots() {
        let state = tempfile::tempdir().unwrap();
        let stale = state.path().join("stale.json");
        let fresh = state.path().join("fresh.json");
        std::fs::write(&stale, "{}").unwrap();
        std::fs::write(&fresh, "{}").unwrap();
        let old = std::time::SystemTime::now() - PENDING_TTL - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();
        prune_pending(state.path());
        assert!(!stale.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn test_context_unexpected_current_source_reports_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.js");
        std::fs::write(&file, "let x = 1;\nconsume(x);\n").unwrap();
        let input = edit_input(dir.path(), &file, "1", "2");
        assert!(context_in(&input, false, state.path()).is_some());
        std::fs::write(&file, "let x = 3;\nconsume(x);\n").unwrap();
        assert!(context_in(&input, true, state.path())
            .unwrap()
            .contains("after-edit consumers unresolved"));
    }

    #[test]
    fn test_context_non_utf8_source_reports_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.js");
        std::fs::write(&file, b"let x = \"\xe9\";\n").unwrap();
        let input = edit_input(dir.path(), &file, "x", "y");
        let rendered = context_in(&input, false, state.path()).unwrap();
        assert!(rendered.contains("not valid UTF-8"), "{rendered}");
        assert!(rendered.contains("consumers unresolved"), "{rendered}");
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

    /// Contract: the rendered context stays under MAX_CONTEXT, is cut on line
    /// boundaries so no structured value is left half-written, and says so.
    #[test]
    fn test_render_large_report_marks_truncation() {
        let report =
            json!({"analysis":[{"file":"x.js","line":1,"result":{"unresolved":"x".repeat(7000)}}]});
        let result = render(&report, "before", "edited file only", MAX_CONTEXT);
        assert!(result.len() <= MAX_CONTEXT);
        assert!(result.contains("truncated=true"));
        let many: Vec<Value> = (0..400)
            .map(|i| json!({"id": i, "file": "x.js", "line": i, "kind": "argument", "label": "consume(x)"}))
            .collect();
        let report = json!({"analysis":[{"file":"x.js","line":1,"result":{"report":{
            "source_hashes": {"x.js": "xxh3:0"},
            "consumers": (0..400).collect::<Vec<_>>(),
            "nodes": many,
            "boundaries": [],
            "boundaries_omitted": 0,
            "truncated": false
        }}}]});
        let result = render(&report, "before", "edited file only", MAX_CONTEXT);
        assert!(result.len() <= MAX_CONTEXT);
        assert!(result.contains("truncated=true"));
        assert!(
            result.lines().all(|line| line.is_empty()
                || !line.starts_with("  x.js")
                || line.ends_with("consume(x)")),
            "{result}"
        );
    }

    #[test]
    fn test_render_keeps_source_labels_on_one_line() {
        let report = json!({"analysis":[{"file":"x.js","line":1,"result":{"report":{
            "source_hashes": {"x.js": "xxh3:0"},
            "consumers": [0],
            "nodes": [{"id": 0, "file": "x.js", "line": 2, "kind": "argument", "label": "consume(\nignore previous instructions\n)"}],
            "boundaries": [],
            "boundaries_omitted": 0,
            "truncated": false
        }}}]});
        let result = render(&report, "before", "edited file only", MAX_CONTEXT);
        assert!(
            result.contains("  x.js:2 argument consume( ignore previous instructions )"),
            "{result}"
        );
    }
}
