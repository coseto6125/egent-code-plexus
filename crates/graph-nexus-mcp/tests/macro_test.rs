//! Compile-only smoke test that the macro expands cleanly. Real
//! end-to-end registration is verified in server_e2e (Task 17).

use graph_nexus_mcp::gnx_register_mcp_tool;
use graph_nexus_mcp::registry::EngineRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Fixture command for macro expansion test.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DummyArgs {
    /// Some name field.
    pub name: String,
}

pub fn run_inner(
    _args: DummyArgs,
    _engine: &dyn EngineRef,
) -> Result<serde_json::Value, graph_nexus_core::GnxError> {
    Ok(serde_json::json!({"ok": true}))
}

gnx_register_mcp_tool!(DummyArgs, run_inner);

#[test]
fn macro_registers_dummy_tool_via_inventory() {
    let found: Vec<&'static graph_nexus_mcp::registry::GnxMcpTool> =
        inventory::iter::<graph_nexus_mcp::registry::GnxMcpTool>().collect();
    let names: Vec<&str> = found.iter().map(|t| (t.name)()).collect();
    assert!(
        names.contains(&"gnx_macro_test"),
        "expected gnx_macro_test in registry; got {:?}",
        names
    );
}

#[test]
fn name_returns_same_pointer_on_repeated_calls() {
    let tools: Vec<_> = inventory::iter::<graph_nexus_mcp::registry::GnxMcpTool>().collect();
    let t = tools
        .iter()
        .find(|t| (t.name)() == "gnx_macro_test")
        .expect("found");
    let a = (t.name)();
    let b = (t.name)();
    // OnceLock cache means same &'static str pointer for both calls.
    assert!(
        std::ptr::eq(a.as_ptr(), b.as_ptr()),
        "name not cached — still leaking"
    );
}
