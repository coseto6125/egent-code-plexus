//! End-to-end: pipe a real MCP JSON-RPC sequence into `gnx mcp serve`'s
//! stdin, read the JSON-RPC response, assert it lists the 8 tools.
//!
//! Protocol notes (rmcp 1.7 stdio transport):
//! - Frames are newline-delimited JSON-RPC (one JSON object per line).
//! - The MCP handshake requires:
//!     1. client → server: `initialize` request
//!     2. server → client: `initialize` response
//!     3. client → server: `notifications/initialized` notification
//!     4. client → server: subsequent requests (e.g. `tools/list`)
//! - stdin EOF signals disconnect; the server flushes buffered responses
//!   before exiting.

use std::io::Write;
use std::process::{Command, Stdio};

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn mcp_server_lists_eight_tools_via_json_rpc() {
    let mut child = Command::new(gnx_bin())
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gnx mcp serve");

    // Write all frames then drop stdin to signal EOF.
    {
        let stdin = child.stdin.as_mut().expect("stdin handle");

        // 1. initialize handshake
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
        // 2. initialized notification (required by MCP spec before any request)
        let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        // 3. tools/list request
        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

        writeln!(stdin, "{init}").unwrap();
        writeln!(stdin, "{initialized}").unwrap();
        writeln!(stdin, "{list}").unwrap();
    }
    // Drop stdin → EOF → server exits its serve loop.
    drop(child.stdin.take());

    let out = child.wait_with_output().expect("wait for gnx mcp serve");
    let stdout = String::from_utf8_lossy(&out.stdout);

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
            stdout.contains(tool),
            "missing {tool} in MCP tools/list response.\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr),
        );
    }
}
