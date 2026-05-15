use graph_nexus_cli::admin::codex_native::{generate_patch, status, Status};
use tempfile::TempDir;

#[test]
fn generate_patch_writes_files_to_expected_paths() {
    let dir = TempDir::new().unwrap();
    let result = generate_patch(dir.path()).unwrap();

    let install_sh = dir.path().join("host-integration/codex-cli/install.sh");
    let gnx_rs = dir.path().join("host-integration/codex-cli/gnx.rs");

    assert!(
        install_sh.exists(),
        "expected install.sh at host-integration/codex-cli/install.sh"
    );
    assert!(
        gnx_rs.exists(),
        "expected gnx.rs at host-integration/codex-cli/gnx.rs"
    );
    // path field should point at the directory containing both files
    assert_eq!(result.path, dir.path().join("host-integration/codex-cli"));
}

#[test]
fn generate_patch_embeds_all_eight_gnx_tools() {
    // Force the linker to keep all 8 command modules so inventory is populated.
    let _ = graph_nexus_cli::commands::context::run_inner;
    let _ = graph_nexus_cli::commands::impact::run_inner;
    let _ = graph_nexus_cli::commands::query::run_inner;
    let _ = graph_nexus_cli::commands::detect_changes::run_inner;
    let _ = graph_nexus_cli::commands::rename::run_inner;
    let _ = graph_nexus_cli::commands::route_map::run_inner;
    let _ = graph_nexus_cli::commands::shape_check::run_inner;
    let _ = graph_nexus_cli::commands::multi_query::run_inner;

    let dir = TempDir::new().unwrap();
    let result = generate_patch(dir.path()).unwrap();

    assert_eq!(
        result.tool_count, 8,
        "expected 8 gnx tools (matches inventory)"
    );

    let gnx_rs = dir.path().join("host-integration/codex-cli/gnx.rs");
    let content = std::fs::read_to_string(&gnx_rs).unwrap();

    for tool in [
        "gnx_context",
        "gnx_impact",
        "gnx_query",
        "gnx_detect_changes",
        "gnx_rename",
        "gnx_route_map",
        "gnx_shape_check",
        "gnx_multi_query",
    ] {
        assert!(
            content.contains(tool),
            "missing tool {tool} in generated gnx.rs"
        );
    }
}

#[test]
fn generate_patch_install_sh_contains_marker() {
    let dir = TempDir::new().unwrap();
    generate_patch(dir.path()).unwrap();

    let install_sh = dir.path().join("host-integration/codex-cli/install.sh");
    let content = std::fs::read_to_string(&install_sh).unwrap();
    assert!(
        content.contains("gnx-integration-marker-v1"),
        "install.sh must contain the marker line"
    );
    assert!(
        content.contains("codex-rs/core/Cargo.toml"),
        "install.sh must reference Cargo.toml"
    );
}

#[test]
fn status_returns_missing_when_codex_repo_doesnt_have_gnx_rs() {
    let dir = TempDir::new().unwrap();
    match status(dir.path()) {
        Status::Missing => {}
        other => panic!("expected Missing, got: {:?}", other),
    }
}

#[test]
fn status_returns_installed_when_marker_line_present() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("codex-rs/core/src/tools/gnx.rs");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "// gnx-integration-marker-v1\nimpl Tool for GnxContext {}\n",
    )
    .unwrap();

    match status(dir.path()) {
        Status::Installed { tool_count: 1 } => {}
        other => panic!("expected Installed{{tool_count=1}}, got: {:?}", other),
    }
}

#[test]
fn status_returns_outdated_when_file_present_but_no_marker() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("codex-rs/core/src/tools/gnx.rs");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "// some unrelated content\n").unwrap();

    match status(dir.path()) {
        Status::Outdated { reason } => {
            assert!(
                reason.contains("marker"),
                "expected marker mention in reason, got: {reason}"
            );
        }
        other => panic!("expected Outdated, got: {:?}", other),
    }
}

#[test]
fn generate_patch_bytes_written_is_nonzero() {
    let dir = TempDir::new().unwrap();
    let result = generate_patch(dir.path()).unwrap();
    assert!(result.bytes_written > 0, "bytes_written must be positive");
}
