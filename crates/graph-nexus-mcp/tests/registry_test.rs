use graph_nexus_mcp::registry::{derive_subcommand, derive_tool_name};

#[test]
fn derive_tool_name_extracts_last_segment_with_gnx_prefix() {
    assert_eq!(derive_tool_name("graph_nexus_cli::commands::context"), "gnx_context");
    assert_eq!(derive_tool_name("graph_nexus_cli::commands::detect_changes"), "gnx_detect_changes");
    assert_eq!(derive_tool_name("graph_nexus_cli::commands::multi_query"), "gnx_multi_query");
}

#[test]
fn derive_subcommand_returns_last_segment_raw() {
    assert_eq!(derive_subcommand("graph_nexus_cli::commands::context"), "context");
    assert_eq!(derive_subcommand("graph_nexus_cli::commands::detect_changes"), "detect_changes");
}
