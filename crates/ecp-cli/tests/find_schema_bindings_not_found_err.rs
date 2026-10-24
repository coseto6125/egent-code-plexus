//! A missing field returns `Err` from `run`; `process::exit(1)` inside the
//! library skipped telemetry and the tantivy join in `main` (and kills any
//! in-process caller, including this test).

use ecp_cli::commands::find_schema_bindings::{run, FindSchemaBindingsArgs};
use ecp_cli::engine::Engine;
use ecp_core::graph_fixture::GraphFixture;

#[test]
fn test_find_schema_bindings_missing_field_returns_err_naming_field() {
    let mut fx = GraphFixture::new();
    fx.func("src/a.py", "helper");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("graph.bin");
    std::fs::write(&path, fx.into_bytes()).unwrap();
    let engine = Engine::load(&path).expect("engine load");
    let args = FindSchemaBindingsArgs {
        field: "User.email".to_string(),
        repo: None,
        format: Some("json".to_string()),
    };
    let err = run(args, &engine).expect_err("missing field must be an Err");
    assert!(err.to_string().contains("User.email"), "{err}");
}
