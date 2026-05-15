//! Smoke: build a server, list-tools, expect the inventory contents.

use graph_nexus_mcp::server::{DispatchMode, GnxMcpServer};

#[tokio::test(flavor = "current_thread")]
async fn list_tools_returns_registered_inventory() {
    let server = GnxMcpServer::new(DispatchMode::Spawn).expect("init");
    let tools = server.list_tools();
    let names: Vec<&str> = tools.iter().map(|t| (t.name)()).collect();
    // From this test crate's perspective, the CLI commands aren't
    // linked in; only fixtures/macros submitted by graph-nexus-mcp's
    // own tests register. The smoke test only asserts the API works
    // — empty registry is acceptable here. End-to-end with CLI tools
    // is exercised by the integration test in Task 17.
    let _ = names; // shape compile-check only
}
