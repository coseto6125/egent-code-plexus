use clap::Parser;
use ecp_cli::cli::Cli;
use ecp_cli::commands::flow::{load_sources, load_sources_at_ref};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

fn query(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args([
            "flow",
            "--file",
            "main.js",
            "--line",
            "1",
            "--column",
            "7",
            "--subject",
            "binding",
            "--format",
            "json",
        ])
        .args(args)
        .current_dir(root)
        .env("ECP_HOME", root.join(".ecp-test"))
        .output()
        .unwrap()
}

fn payload(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn test_flow_unindexed_source_reports_consumers_without_graph() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join("main.js"),
        "const x = 1;\nconst y = x + 2;\nconsume(y);\n",
    )
    .unwrap();
    let report = payload(query(repo.path(), &["--no-cache"]));
    assert!(
        !report["consumers"].as_array().unwrap().is_empty(),
        "{report}"
    );
    assert_eq!(report["source_scope"]["index_required"], false);
    assert!(!repo.path().join(".ecp/graph.bin").exists());
}

#[test]
fn test_flow_source_change_invalidates_cached_dependencies() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("main.js"), "const x = 1;\nconsume(x);\n").unwrap();
    assert_eq!(payload(query(repo.path(), &[]))["cache_hit"], false);
    assert_eq!(payload(query(repo.path(), &[]))["cache_hit"], true);
    std::fs::write(repo.path().join("main.js"), "const x = 2;\nconsume(x);\n").unwrap();
    assert_eq!(payload(query(repo.path(), &[]))["cache_hit"], false);
}

#[test]
fn test_flow_dependency_change_invalidates_cache() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("main.js"), "const x = 1;\nconsume(x);\n").unwrap();
    std::fs::write(repo.path().join("dep.js"), "export const z = 1;").unwrap();
    payload(query(repo.path(), &[]));
    std::fs::write(repo.path().join("dep.js"), "export const z = 2;").unwrap();
    assert_eq!(payload(query(repo.path(), &[]))["cache_hit"], false);
}

#[test]
fn test_flow_overlay_uses_unsaved_source_without_writing_it() {
    let repo = tempfile::tempdir().unwrap();
    let original = "const x = 1;\n";
    std::fs::write(repo.path().join("main.js"), original).unwrap();
    let overlay = repo.path().join("overlay.json");
    std::fs::write(
        &overlay,
        json!({"main.js":"const x = 1;\nconsume(x);\n"}).to_string(),
    )
    .unwrap();
    let report = payload(query(
        repo.path(),
        &["--overlay", overlay.to_str().unwrap(), "--no-cache"],
    ));
    assert_eq!(report["source_kind"], "overlay");
    assert!(
        !report["consumers"].as_array().unwrap().is_empty(),
        "{report}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("main.js")).unwrap(),
        original
    );
}

#[test]
fn test_flow_overlay_parent_path_is_rejected() {
    let repo = tempfile::tempdir().unwrap();
    let overlay = repo.path().join("overlay.json");
    std::fs::write(
        &overlay,
        json!({"../outside.js":"const x = 1;"}).to_string(),
    )
    .unwrap();
    assert!(load_sources(repo.path(), Some(&overlay)).is_err());
}

#[test]
fn test_load_sources_oversized_overlay_rejects_before_json_decode() {
    let repo = tempfile::tempdir().unwrap();
    let overlay = repo.path().join("overlay.json");
    std::fs::File::create(&overlay)
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    let error = load_sources(repo.path(), Some(&overlay)).unwrap_err();
    assert!(
        error.to_string().contains("overlay JSON exceeds"),
        "{error}"
    );
}

#[test]
fn test_load_sources_python_stub_uses_engine_language_support() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join("model.pyi"),
        "def consume(value: int) -> int: ...\n",
    )
    .unwrap();
    assert_eq!(
        load_sources(repo.path(), None).unwrap().files[0].path,
        "model.pyi"
    );
}

#[test]
fn test_flow_zero_position_is_rejected() {
    assert!(Cli::try_parse_from(["ecp", "flow", "--file", "a.js", "--line", "0"]).is_err());
}

#[test]
fn test_flow_mcp_schema_exposes_position_and_selection_without_graph() {
    use clap::CommandFactory;
    let tools = ecp_mcp::schema::ecp_tools(&Cli::command());
    let tool = tools
        .iter()
        .find(|t| t.name == "ecp_flow")
        .expect("flow MCP tool");
    assert_eq!(tool.schema["properties"]["line"]["type"], "integer");
    assert!(tool.schema["required"]
        .as_array()
        .unwrap()
        .contains(&json!("file")));
    let cli = Cli::try_parse_from(["ecp", "flow", "--file", "main.js", "--line", "1"]).unwrap();
    assert!(!cli.command.needs_graph());
}

#[test]
fn test_flow_mcp_arguments_produce_same_evidence_as_cli() {
    use clap::CommandFactory;
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("main.js"), "const x = 1;\nconsume(x);\n").unwrap();
    let tools = ecp_mcp::schema::ecp_tools(&Cli::command());
    let tool = tools.iter().find(|t| t.name == "ecp_flow").unwrap();
    let argv = ecp_mcp::spawn::build_argv(tool, &json!({"file":"main.js", "line":1, "column":7, "subject":"binding", "format":"json", "no_cache":true})).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .arg(&tool.subcommand)
        .args(argv)
        .current_dir(repo.path())
        .env("ECP_HOME", repo.path().join(".ecp-test"))
        .output()
        .unwrap();
    assert_eq!(
        payload(output),
        payload(query(repo.path(), &["--no-cache"]))
    );
    let review = tools.iter().find(|t| t.name == "ecp_review").unwrap();
    assert_eq!(
        review.schema["properties"]["include"]["enum"],
        json!(["flow"])
    );
    let argv =
        ecp_mcp::spawn::build_argv(review, &json!({"since":"HEAD", "include":"flow"})).unwrap();
    assert!(Cli::try_parse_from(
        ["ecp".to_owned(), "review".to_owned()]
            .into_iter()
            .chain(argv)
    )
    .is_ok());
}

#[test]
fn test_load_sources_at_ref_preserves_baseline_and_unusual_paths() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "flow@test"],
        vec!["config", "user.name", "Flow test"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }
    let file = "space name.js";
    std::fs::write(root.join(file), "const x = 1;").unwrap();
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-qm", "baseline"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    std::fs::write(root.join(file), "const x = 2;").unwrap();
    let baseline = load_sources_at_ref(root, "HEAD").unwrap();
    assert_eq!(baseline.files.len(), 1);
    assert_eq!(baseline.files[0].path, file);
    assert_eq!(baseline.files[0].source, "const x = 1;");
    assert_eq!(
        load_sources(root, None).unwrap().files[0].source,
        "const x = 2;"
    );
    assert!(load_sources_at_ref(root, "--all").is_err());
}

#[test]
fn test_load_sources_tracked_ignored_matches_baseline_without_fake_deletion() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "flow@test"],
        vec!["config", "user.name", "Flow test"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(root.join(".gitignore"), "*.js\n").unwrap();
    std::fs::write(root.join("tracked.js"), "const x = 1;\nconsume(x);\n").unwrap();
    std::fs::create_dir(root.join("node_modules")).unwrap();
    std::fs::write(root.join("node_modules/excluded.js"), "const excluded = 1;").unwrap();
    for args in [vec!["add", "-f", "."], vec!["commit", "-qm", "baseline"]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(root.join("untracked.js"), "const ignored = 1;").unwrap();
    let before = load_sources_at_ref(root, "HEAD").unwrap();
    let after = load_sources(root, None).unwrap();
    assert_eq!(
        after
            .files
            .iter()
            .map(|source| source.path.as_str())
            .collect::<Vec<_>>(),
        ["tracked.js"]
    );
    let review = ecp_cli::commands::review::flow::compare(&before.files, &after.files, None);
    assert!(
        review["analysis"].as_array().unwrap().is_empty(),
        "{review}"
    );
    std::fs::remove_file(root.join("tracked.js")).unwrap();
    assert!(load_sources(root, None).unwrap().files.is_empty());
}

#[cfg(unix)]
#[test]
fn test_load_sources_tracked_symlink_ancestor_rejects_outside_scope() {
    let repo = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = repo.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.js"), "const x = 1;").unwrap();
    for args in [vec!["init", "-q"], vec!["add", "."]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }
    std::fs::rename(root.join("src"), root.join("original")).unwrap();
    std::fs::write(outside.path().join("main.js"), "const outside = 2;").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("src")).unwrap();
    let error = load_sources(root, None).unwrap_err();
    assert!(error.to_string().contains("outside --repo"), "{error}");
}

/// Contract: an unreadable source is excluded and reported, never fatal.
/// Before this fix one latin-1 file failed every flow query in the repo.
#[test]
fn test_load_sources_non_utf8_file_is_skipped_with_boundary() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("ok.js"), "const x = 1;\n").unwrap();
    std::fs::write(repo.path().join("latin1.js"), b"let z = \"\xe9\";\n").unwrap();
    let loaded = load_sources(repo.path(), None).unwrap();
    assert_eq!(
        loaded
            .files
            .iter()
            .map(|source| source.path.as_str())
            .collect::<Vec<_>>(),
        ["ok.js"]
    );
    assert_eq!(loaded.skipped.len(), 1);
    assert_eq!(loaded.skipped[0].file, "latin1.js");
    assert_eq!(loaded.skipped[0].kind, "unreadable_source");
}

#[test]
fn test_load_sources_tracked_non_utf8_reports_one_boundary() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("latin1.js"), b"\xff").unwrap();
    for args in [vec!["init", "-q"], vec!["add", "latin1.js"]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
    }
    let loaded = load_sources(repo.path(), None).unwrap();
    assert!(loaded.files.is_empty());
    assert_eq!(loaded.skipped.len(), 1);
    assert_eq!(loaded.skipped[0].file, "latin1.js");
}

#[test]
fn test_load_sources_overlay_repairs_non_utf8_removes_only_replaced_boundary() {
    let repo = tempfile::tempdir().unwrap();
    for path in ["repaired.js", "unreadable.js"] {
        std::fs::write(repo.path().join(path), b"\xff").unwrap();
    }
    let overlay = repo.path().join("overlay.json");
    std::fs::write(
        &overlay,
        json!({"./repaired.js": "const x = 1;"}).to_string(),
    )
    .unwrap();
    let loaded = load_sources(repo.path(), Some(&overlay)).unwrap();
    assert_eq!(loaded.files.len(), 1);
    assert_eq!(loaded.files[0].path, "repaired.js");
    assert_eq!(loaded.files[0].source, "const x = 1;");
    assert_eq!(loaded.skipped.len(), 1);
    assert_eq!(loaded.skipped[0].file, "unreadable.js");
    assert_eq!(
        std::fs::read(repo.path().join("repaired.js")).unwrap(),
        b"\xff"
    );
}

#[test]
fn test_load_sources_overlay_replaces_non_utf8_byte_budget() {
    let repo = tempfile::tempdir().unwrap();
    let mut source = std::fs::File::create(repo.path().join("main.js")).unwrap();
    source.write_all(b"\xff").unwrap();
    source.set_len(32 * 1024 * 1024).unwrap();
    drop(source);
    let overlay = repo.path().join("overlay.json");
    std::fs::write(&overlay, json!({"main.js": "const x = 1;"}).to_string()).unwrap();
    let loaded = load_sources(repo.path(), Some(&overlay)).unwrap();
    assert_eq!(loaded.files.len(), 1);
    assert_eq!(loaded.files[0].source, "const x = 1;");
    assert!(loaded.skipped.is_empty());
}

#[test]
fn test_load_sources_tracked_readable_after_skipped_rejects_file_budget_overflow() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join(".gitignore"), "tracked.js\n").unwrap();
    std::fs::write(repo.path().join("tracked.js"), "const x = 1;").unwrap();
    for args in [vec!["init", "-q"], vec!["add", "-f", "tracked.js"]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
    }
    for index in 0..2047 {
        std::fs::write(repo.path().join(format!("unreadable_{index}.js")), b"\xff").unwrap();
    }
    let loaded = load_sources(repo.path(), None).unwrap();
    assert_eq!(loaded.files.len(), 1);
    assert_eq!(loaded.skipped.len(), 2047);
    std::fs::write(repo.path().join("one_more.js"), b"\xff").unwrap();
    let error = load_sources(repo.path(), None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("flow source scope exceeds 2048 files"),
        "{error}"
    );
}
