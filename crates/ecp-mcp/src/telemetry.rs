//! Best-effort jsonl telemetry writer for MCP call_tool dispatch.
//!
//! Each MCP tool invocation appends one line to
//! `~/.ecp/telemetry/<repo>/calls.jsonl`.
//!
//! Design constraints:
//! - Write path is entirely best-effort: all I/O errors are silently dropped.
//! - Never blocks or panics the MCP dispatch path.
//! - Schema is **unstable (v1)** — do not commit to field stability yet.

use std::io::Write as _;
use std::path::PathBuf;

/// One record appended per `call_tool` invocation.
/// Fields are kept flat and minimal so appending new optional fields in v2 is
/// backward-compatible (old readers will simply ignore unknown keys).
#[derive(serde::Serialize)]
pub struct CallRecord<'a> {
    /// RFC3339 UTC timestamp of the call start.
    pub ts: &'a str,
    /// MCP tool name (e.g. `"ecp_inspect"`).
    pub tool: &'a str,
    /// Wall-clock duration of the `run_spawn` call in milliseconds.
    pub duration_ms: u64,
    /// `true` if the spawn returned `Ok(_)`, `false` on `Err(_)`.
    pub ok: bool,
}

/// Derive the per-repo telemetry directory from the current working directory.
///
/// Uses `ecp_core::registry::resolve_home_ecp()` as the base so the path
/// mirrors where the rest of ecp state lives.
///
/// Returns `None` if the cwd cannot be determined or has no usable name
/// (extremely unlikely in practice; silently discards telemetry in that case).
fn telemetry_dir() -> Option<PathBuf> {
    let repo_name = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))?;
    let base = ecp_core::registry::resolve_home_ecp();
    Some(base.join("telemetry").join(repo_name))
}

/// Append one jsonl record to `~/.ecp/telemetry/<repo>/calls.jsonl`.
/// All I/O errors are silently discarded — telemetry failure MUST NOT
/// impact MCP dispatch.
pub fn append(record: &CallRecord<'_>) {
    let Some(dir) = telemetry_dir() else { return };
    let _ = append_inner(record, &dir);
}

/// Append to an explicit directory (used by tests to control the write path).
pub fn append_to(record: &CallRecord<'_>, dir: &std::path::Path) {
    let _ = append_inner(record, dir);
}

fn append_inner(record: &CallRecord<'_>, dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("calls.jsonl");
    let line = serde_json::to_string(record).map_err(std::io::Error::other)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{line}")
}

/// Format a `SystemTime` as RFC3339 UTC, e.g. `2026-05-23T07:30:00Z`.
/// Stdlib-only; no chrono dependency in ecp-mcp.
pub fn rfc3339_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    unix_secs_to_rfc3339(secs)
}

/// Convert Unix seconds → `YYYY-MM-DDTHH:MM:SSZ` (UTC).
/// Hand-rolled to stay stdlib-only.
pub(crate) fn unix_secs_to_rfc3339(secs: u64) -> String {
    // Days since 1970-01-01
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Gregorian calendar algorithm: days since 1970-01-01 → (year, month, day).
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // Shift epoch to 1 Mar 0000 for easier leap-year arithmetic.
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month index (Mar=0)
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_secs_known_date() {
        // 2026-05-23T00:00:00Z = 1779494400
        assert_eq!(unix_secs_to_rfc3339(1779494400), "2026-05-23T00:00:00Z");
        // 1970-01-01T00:00:00Z = 0
        assert_eq!(unix_secs_to_rfc3339(0), "1970-01-01T00:00:00Z");
    }
}
