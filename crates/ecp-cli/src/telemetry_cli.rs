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

/// Map an error to a small closed taxonomy.
///
/// Dispatches on the structured `EcpError` variant first (covers known
/// no-symbol / ambiguous-symbol / git-diff failures), then falls back to a
/// substring match for free-form variants (`InvalidArgument`, `Output`,
/// `Rkyv`). `InvalidArgument` is the dumping ground for clap-level user
/// input errors (`unknown agent 'foo'`, `--graph path does not exist`,
/// `exactly one host flag required`); those collapse to `user-input` so
/// `ecp usage --failures` separates "user typed something invalid" from
/// "ecp blew up". `ok` is still `false` — err_rate keeps reflecting how
/// often invocations don't produce a useful result.
pub fn classify_error(e: &EcpError) -> &'static str {
    match e {
        EcpError::SymbolNotFound { .. } | EcpError::AmbiguousSymbol { .. } => "no-such-symbol",
        EcpError::GraphNotFound { .. } => "graph-load-failed",
        EcpError::GitDiff { .. } => "git-diff",
        EcpError::Io(_) => "io",
        EcpError::Rkyv(_) => "graph-load-failed",
        EcpError::InvalidArgument(msg) => classify_invalid_argument(msg),
        EcpError::Output(msg) | EcpError::Serialization(msg) => classify_freeform(msg),
    }
}

/// `InvalidArgument` carries both clap-validation strings ("unknown agent
/// 'foo'", "missing --host") and lower-level guard strings ("registry lock
/// timeout"). Distinguish by signal phrases — user-input messages tend to
/// name an expected value; internal ones name a system resource.
fn classify_invalid_argument(msg: &str) -> &'static str {
    let m = msg.to_ascii_lowercase();
    if m.contains("cypher") && (m.contains("parse") || m.contains("label") || m.contains("syntax"))
    {
        return "cypher-parse";
    }
    if m.contains("not found") || m.contains("no symbol") || m.contains("no such symbol") {
        return "no-such-symbol";
    }
    if m.contains("stale") || m.contains("older than head") {
        return "index-stale";
    }
    if (m.contains("load") && m.contains("graph"))
        || m.contains("registry lock")
        || m.contains("corrupt")
    {
        return "graph-load-failed";
    }
    // Heuristic for "user typed something wrong": clap-validation strings.
    // Captures `unknown agent 'x'`, `--graph path does not exist`,
    // `exactly one host flag required`, `expected <choices>`, `usage:`.
    if m.contains("unknown ")
        || m.contains("expected ")
        || m.contains("does not exist")
        || m.contains("required")
        || m.contains("invalid value")
        || m.contains("usage:")
    {
        return "user-input";
    }
    "other"
}

/// Free-form `Output` / `Serialization` payloads. Same heuristics as the
/// pre-v2 substring rule, no user-input bucket here (these variants are
/// emitted by internal code paths, not clap).
fn classify_freeform(msg: &str) -> &'static str {
    let m = msg.to_ascii_lowercase();
    if m.contains("cypher") && (m.contains("parse") || m.contains("label") || m.contains("syntax"))
    {
        return "cypher-parse";
    }
    if m.contains("not found") || m.contains("no symbol") || m.contains("no such symbol") {
        return "no-such-symbol";
    }
    if m.contains("stale") || m.contains("older than head") {
        return "index-stale";
    }
    if (m.contains("load") && m.contains("graph"))
        || m.contains("registry lock")
        || m.contains("corrupt")
    {
        return "graph-load-failed";
    }
    "other"
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
    fn classify_user_input_clap_messages() {
        // Three real failure modes seen in the v2 telemetry sample.
        assert_eq!(
            classify_error(&EcpError::InvalidArgument(
                "unknown agent 'unknown-agent' — expected claude, codex, or gemini".into()
            )),
            "user-input"
        );
        assert_eq!(
            classify_error(&EcpError::InvalidArgument(
                "Error: --graph path does not exist: /tmp/x.bin".into()
            )),
            "user-input"
        );
        assert_eq!(
            classify_error(&EcpError::InvalidArgument(
                "ecp hook: exactly one host flag required (e.g. --claude-code)".into()
            )),
            "user-input"
        );
    }

    #[test]
    fn classify_structured_variants() {
        assert_eq!(
            classify_error(&EcpError::SymbolNotFound {
                uid: "Foo::bar".into()
            }),
            "no-such-symbol"
        );
        assert_eq!(
            classify_error(&EcpError::AmbiguousSymbol {
                name: "Foo".into(),
                count: 3
            }),
            "no-such-symbol"
        );
        assert_eq!(
            classify_error(&EcpError::GraphNotFound {
                path: std::path::PathBuf::from("/x")
            }),
            "graph-load-failed"
        );
        assert_eq!(
            classify_error(&EcpError::GitDiff {
                reason: "no such ref".into()
            }),
            "git-diff"
        );
    }

    #[test]
    fn classify_user_input_does_not_swallow_internal_invalid_argument() {
        // Internal guard messages must keep their specific bucket, not fall
        // through to "user-input".
        assert_eq!(
            classify_error(&EcpError::InvalidArgument(
                "registry lock timeout after 5s".into()
            )),
            "graph-load-failed"
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
