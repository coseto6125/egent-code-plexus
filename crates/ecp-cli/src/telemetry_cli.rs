//! CLI-side telemetry recorder. One CallRecord per `ecp <cmd>` invocation
//! (success or failure) is appended to `~/.ecp/telemetry/<repo>/cli-calls.jsonl`.
//! Best-effort; never affects the command's own exit code (see main.rs).

use ecp_core::telemetry::{append_record, CallRecord};
use ecp_core::time::rfc3339_now;
use ecp_core::EcpError;
use std::path::PathBuf;

pub const CLI_TELEMETRY_FILE: &str = "cli-calls.jsonl";

/// Map an error to a small closed taxonomy. Substring match on the message —
/// the error type lacks structured variants for these. Unknown → "other".
pub fn classify_error(e: &EcpError) -> &'static str {
    let msg = e.to_string().to_ascii_lowercase();
    if msg.contains("cypher")
        && (msg.contains("parse") || msg.contains("label") || msg.contains("syntax"))
    {
        "cypher-parse"
    } else if msg.contains("not found")
        || msg.contains("no symbol")
        || msg.contains("no such symbol")
    {
        "no-such-symbol"
    } else if msg.contains("stale") || msg.contains("older than head") {
        "index-stale"
    } else if (msg.contains("load") && msg.contains("graph"))
        || msg.contains("registry lock")
        || msg.contains("corrupt")
    {
        "graph-load-failed"
    } else {
        "other"
    }
}

/// Disabled by `ECP_NO_TELEMETRY` (any value) or config `telemetry.cli=false`.
pub fn is_enabled() -> bool {
    if std::env::var_os("ECP_NO_TELEMETRY").is_some() {
        return false;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ecp_core::config::load(&cwd)
        .map(|c| c.telemetry.cli)
        .unwrap_or(true)
}

/// Resolve `~/.ecp/telemetry/<repo_key>/` for the current dir. None on failure.
fn telemetry_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let key = crate::repo_identity::repo_dir_name_for_cwd(&cwd).ok()?;
    Some(
        ecp_core::registry::resolve_home_ecp()
            .join("telemetry")
            .join(key),
    )
}

/// Build + append the record. `label` is the subcommand name; `err` carries the
/// classified kind on failure. No-op when disabled or repo key unresolved.
pub fn record(label: &str, duration_ms: u64, err: Option<&EcpError>) {
    if !is_enabled() {
        return;
    }
    let Some(dir) = telemetry_dir() else { return };
    let kind = err.map(classify_error);
    let ts = rfc3339_now();
    let rec = CallRecord {
        ts: &ts,
        tool: label,
        duration_ms,
        ok: err.is_none(),
        source: "cli",
        error_kind: kind,
    };
    append_record(&dir, CLI_TELEMETRY_FILE, &rec);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::EcpError;

    #[test]
    fn classify_maps_known_errors() {
        assert_eq!(
            classify_error(&EcpError::InvalidArgument("symbol 'foo' not found".into())),
            "no-such-symbol"
        );
        assert_eq!(
            classify_error(&EcpError::InvalidArgument(
                "cypher parse error near X".into()
            )),
            "cypher-parse"
        );
        assert_eq!(
            classify_error(&EcpError::InvalidArgument("totally novel boom".into())),
            "other"
        );
    }

    #[test]
    fn opt_out_via_env() {
        std::env::set_var("ECP_NO_TELEMETRY", "1");
        assert!(!is_enabled());
        std::env::remove_var("ECP_NO_TELEMETRY");
    }
}
