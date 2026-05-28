//! Shared telemetry record + best-effort jsonl appender.
//!
//! One [`CallRecord`] is appended per invocation — by the CLI (one process
//! per command, file `cli-calls.jsonl`) and by the MCP server (long-lived,
//! file `calls.jsonl`, via its own cached-writer wrapper in ecp-mcp).
//!
//! Schema is **unstable (v1)**. New fields are append-only and optional on
//! read (`#[serde(default)]`) so existing files stay parseable.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

/// One record appended per invocation. CLI and MCP share this exact struct.
///
/// New fields are append-only and `skip_serializing_if = "Option::is_none"` so
/// old readers see `None` and old files still parse. Order of fields is the
/// jsonl column order on disk — keep mandatory v1 fields first.
#[derive(serde::Serialize)]
pub struct CallRecord<'a> {
    /// RFC3339 UTC timestamp of the call start.
    pub ts: &'a str,
    /// Top-level verb. CLI: `"inspect"` / `"admin"` / `"hook"`. MCP: tool name (`"ecp_inspect"`).
    pub tool: &'a str,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// `true` on success, `false` on error.
    pub ok: bool,
    /// `"cli"` or `"mcp"`. Distinguishes the two invocation paths.
    pub source: &'a str,
    /// Failure class (e.g. `"no-such-symbol"`); `None` when `ok == true`.
    pub error_kind: Option<&'a str>,
    /// Nested CLI subcommand (e.g. tool=`"admin"` subcommand=`"gc"`). `None` for
    /// flat verbs / MCP. Added v2 — readers must tolerate absence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcommand: Option<&'a str>,
    /// Truncated raw error message (≤200 chars, NFC, single-line). `None` when
    /// `ok == true`. Lets `ecp usage --failures` show what actually broke
    /// without leaking long stack traces. Added v2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_msg: Option<&'a str>,
}

/// Append one jsonl line to `dir/filename`. Best-effort: all I/O errors are
/// silently dropped — telemetry MUST NOT affect the caller's result. Single
/// `O_APPEND` write of a sub-PIPE_BUF line is atomic under POSIX, so no lock.
pub fn append_record(dir: &Path, filename: &str, record: &CallRecord<'_>) {
    let Ok(line) = serde_json::to_string(record) else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(filename))
    {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_record_serializes_all_fields() {
        let r = CallRecord {
            ts: "2026-05-27T07:00:00Z",
            tool: "inspect",
            duration_ms: 6,
            ok: true,
            source: "cli",
            error_kind: None,
            subcommand: None,
            error_msg: None,
        };
        let line = serde_json::to_string(&r).unwrap();
        assert!(line.contains(r#""source":"cli""#));
        assert!(line.contains(r#""tool":"inspect""#));
        assert!(line.contains(r#""error_kind":null"#));
        // Optional v2 fields with None must be omitted, not emitted as null.
        assert!(!line.contains("subcommand"));
        assert!(!line.contains("error_msg"));
    }

    #[test]
    fn call_record_v2_fields_round_trip() {
        let r = CallRecord {
            ts: "2026-05-27T07:00:00Z",
            tool: "admin",
            duration_ms: 12,
            ok: false,
            source: "cli",
            error_kind: Some("graph-load-failed"),
            subcommand: Some("gc"),
            error_msg: Some("registry lock timeout after 5s"),
        };
        let line = serde_json::to_string(&r).unwrap();
        assert!(line.contains(r#""subcommand":"gc""#));
        assert!(line.contains(r#""error_msg":"registry lock timeout after 5s""#));
    }

    #[test]
    fn append_record_writes_one_line() {
        let dir = std::env::temp_dir().join(format!("ecp-tlm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let r = CallRecord {
            ts: "2026-05-27T07:00:00Z",
            tool: "find",
            duration_ms: 4,
            ok: false,
            source: "cli",
            error_kind: Some("no-such-symbol"),
            subcommand: None,
            error_msg: Some("symbol 'foo' not found"),
        };
        append_record(&dir, "cli-calls.jsonl", &r);
        let body = std::fs::read_to_string(dir.join("cli-calls.jsonl")).unwrap();
        assert_eq!(body.lines().count(), 1);
        assert!(body.contains(r#""error_kind":"no-such-symbol""#));
        assert!(body.contains(r#""error_msg":"symbol 'foo' not found""#));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
