use graph_nexus_mcp::registry::{derive_subcommand, derive_tool_name};

#[test]
fn derive_tool_name_extracts_last_segment_with_gnx_prefix() {
    assert_eq!(
        derive_tool_name("graph_nexus_cli::commands::context"),
        "gnx_context"
    );
    assert_eq!(
        derive_tool_name("graph_nexus_cli::commands::detect_changes"),
        "gnx_detect_changes"
    );
    assert_eq!(
        derive_tool_name("graph_nexus_cli::commands::multi_query"),
        "gnx_multi_query"
    );
}

#[test]
fn derive_subcommand_returns_last_segment_raw() {
    assert_eq!(
        derive_subcommand("graph_nexus_cli::commands::context"),
        "context"
    );
    assert_eq!(
        derive_subcommand("graph_nexus_cli::commands::detect_changes"),
        "detect_changes"
    );
}

#[test]
fn derive_tool_name_handles_module_path_without_colons() {
    // module_path!() in a top-level test crate yields no `::` separator.
    assert_eq!(derive_tool_name("just_a_name"), "gnx_just_a_name");
}

#[test]
fn derive_tool_name_handles_empty_module_path() {
    // Pathological — included for completeness.
    assert_eq!(derive_tool_name(""), "gnx_");
}

#[test]
fn derive_subcommand_handles_module_path_without_colons() {
    assert_eq!(derive_subcommand("standalone"), "standalone");
}
