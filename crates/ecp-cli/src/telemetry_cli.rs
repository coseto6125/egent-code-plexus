//! CLI-side telemetry recorder. One CallRecord per `ecp <cmd>` invocation
//! (success or failure) is appended to `~/.ecp/telemetry/<repo>/cli-calls.jsonl`.
//! Best-effort; never affects the command's own exit code (see main.rs).

use ecp_core::telemetry::{append_record, CallRecord};
use ecp_core::time::rfc3339_now;
use ecp_core::EcpError;
use std::path::PathBuf;

pub const CLI_TELEMETRY_FILE: &str = "cli-calls.jsonl";

/// Max bytes of `error_msg` we keep on disk. 200 is enough to identify a
/// failure mode (`"registry lock timeout after 5s"`, `"failed to load graph:
/// corrupt header at offset 0x40"`) without bloating jsonl with stack traces.
const ERROR_MSG_CAP: usize = 200;

/// Squash newlines/tabs to spaces and clip to `ERROR_MSG_CAP` bytes on a char
/// boundary. Telemetry lines must be single-line jsonl, and serde already
/// escapes control chars — this is purely about readability + size.
fn sanitize_error_msg(raw: &str) -> String {
    let one_line: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = one_line.trim();
    if trimmed.len() <= ERROR_MSG_CAP {
        return trimmed.to_string();
    }
    let mut end = ERROR_MSG_CAP;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

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

/// True when the canonical cwd lives under the system temp dir. Ephemeral
/// shells (`mktemp -d`, pytest tmpdirs, throwaway repros) would otherwise
/// each spawn their own `~/.ecp/telemetry/tmpXXX__<hash>/` bucket that lives
/// forever — see Round 1 of the diagnosable-telemetry investigation.
fn is_ephemeral_cwd(cwd: &std::path::Path) -> bool {
    let Ok(canon) = std::fs::canonicalize(cwd) else {
        return false;
    };
    let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
    canon.starts_with(&tmp)
}

/// Resolve `~/.ecp/telemetry/<repo_key>/` for the current dir. Ephemeral cwds
/// (anything under the system temp dir) collapse to a shared `_ephemeral__`
/// bucket so throwaway shells don't pollute the dashboard with per-dir junk.
fn telemetry_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let key = if is_ephemeral_cwd(&cwd) {
        "_ephemeral__".to_string()
    } else {
        crate::repo_identity::repo_dir_name_for_cwd(&cwd).ok()?
    };
    Some(
        ecp_core::registry::resolve_home_ecp()
            .join("telemetry")
            .join(key),
    )
}

/// Build + append the record. `tool` is the top-level verb, `subcommand` the
/// optional nested verb (`Some("gc")` for `ecp admin gc`); `err` carries the
/// classified kind on failure. No-op when disabled or repo key unresolved.
pub fn record(tool: &str, subcommand: Option<&str>, duration_ms: u64, err: Option<&EcpError>) {
    if !is_enabled() {
        return;
    }
    let Some(dir) = telemetry_dir() else { return };
    let kind = err.map(classify_error);
    let msg = err.map(|e| sanitize_error_msg(&e.to_string()));
    let ts = rfc3339_now();
    let rec = CallRecord {
        ts: &ts,
        tool,
        duration_ms,
        ok: err.is_none(),
        source: "cli",
        error_kind: kind,
        subcommand,
        error_msg: msg.as_deref(),
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

    #[test]
    fn sanitize_error_msg_collapses_newlines_and_clips() {
        let raw = "boom\n  with\ttabs\nand a really long tail ".to_string() + &"x".repeat(500);
        let out = sanitize_error_msg(&raw);
        assert!(!out.contains('\n'));
        assert!(!out.contains('\t'));
        // 200 cap + ellipsis = at most 200 ASCII bytes of content + "…" (3 bytes).
        assert!(out.len() <= ERROR_MSG_CAP + 3);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn sanitize_error_msg_short_passthrough() {
        assert_eq!(
            sanitize_error_msg("symbol 'foo' not found"),
            "symbol 'foo' not found"
        );
    }

    #[test]
    fn ephemeral_cwd_detected_for_temp_dir() {
        let tmp = std::env::temp_dir();
        assert!(is_ephemeral_cwd(&tmp));
    }

    #[test]
    fn ephemeral_cwd_false_for_home() {
        // $HOME is never under tempdir on supported platforms.
        if let Some(home) = std::env::var_os("HOME") {
            let h = std::path::PathBuf::from(home);
            assert!(!is_ephemeral_cwd(&h));
        }
    }
}
