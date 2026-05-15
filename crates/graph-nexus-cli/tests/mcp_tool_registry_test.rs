//! Verifies all 8 in-scope commands self-registered via inventory.
//! A new 9th command added later: this test updates with one row.

#[test]
fn expected_eight_tools_present() {
    // Ensure all command modules are linked in.
    let _ = graph_nexus_cli::commands::context::run_inner;
    let _ = graph_nexus_cli::commands::impact::run_inner;
    let _ = graph_nexus_cli::commands::query::run_inner;
    let _ = graph_nexus_cli::commands::detect_changes::run_inner;
    let _ = graph_nexus_cli::commands::rename::run_inner;
    let _ = graph_nexus_cli::commands::route_map::run_inner;
    let _ = graph_nexus_cli::commands::shape_check::run_inner;
    let _ = graph_nexus_cli::commands::multi_query::run_inner;

    use graph_nexus_mcp::registry::GnxMcpTool;
    let names: std::collections::BTreeSet<&str> =
        inventory::iter::<GnxMcpTool>().map(|t| (t.name)()).collect();
    let expected = [
        "gnx_context",
        "gnx_detect_changes",
        "gnx_impact",
        "gnx_multi_query",
        "gnx_query",
        "gnx_rename",
        "gnx_route_map",
        "gnx_shape_check",
    ];
    for tool in expected {
        assert!(names.contains(tool), "missing {tool} in registry; got {names:?}");
    }
}
