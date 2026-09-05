//! Stdlib-only date / RFC3339 helpers.
//!
//! Centralises the Gregorian calendar algorithm + `YYYY-MM-DDTHH:MM:SSZ`
//! formatter used by telemetry write/read paths. Prior to consolidation,
//! three near-identical copies of `days_to_ymd` / `unix_secs_to_rfc3339`
//! lived in `crates/ecp-mcp/src/telemetry.rs`,
//! `crates/ecp-cli/src/commands/insight.rs`, and the matching test files.
//! Any calendar fix would have needed to land in all three.
//!
//! Kept stdlib-only on purpose — the only consumers are best-effort
//! telemetry / observability paths, and pulling chrono into `ecp-core`
//! would add a transitive dependency for every downstream crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// `SystemTime::now()` as Unix seconds (0 if the clock reads pre-epoch).
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `SystemTime::now()` rendered as RFC3339 UTC, e.g. `2026-05-23T07:30:00Z`.
pub fn rfc3339_now() -> String {
    unix_secs_to_rfc3339(now_unix_secs())
}

/// Convert Unix seconds → `YYYY-MM-DDTHH:MM:SSZ` (UTC, second precision).
pub fn unix_secs_to_rfc3339(secs: u64) -> String {
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` → Unix seconds. Returns `None` on any
/// shape mismatch (length, digit positions, value range). Designed for
/// jsonl read paths that tolerate occasional malformed lines.
pub fn parse_rfc3339_secs(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    let year = ascii_digits(b, 0, 4)?;
    let month = ascii_digits(b, 5, 7)?;
    let day = ascii_digits(b, 8, 10)?;
    let hh = ascii_digits(b, 11, 13)?;
    let mm = ascii_digits(b, 14, 16)?;
    let ss = ascii_digits(b, 17, 19)?;
    let days = ymd_to_days(year, month, day)?;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

/// `b[start..end]` read as ASCII digits. `None` when any byte is not one.
///
/// Reading bytes rather than `&s[start..end]` is what keeps the shape gate
/// honest: a `str` slice panics when its bound lands inside a multi-byte
/// char, so a timestamp like `202é01-01T00:00:00Z` — 20 bytes, `T` at 10,
/// `Z` at 19, so it passes the gate — used to abort the whole read instead
/// of being rejected as malformed.
fn ascii_digits(b: &[u8], start: usize, end: usize) -> Option<u64> {
    let mut n = 0u64;
    for &c in &b[start..end] {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n * 10 + u64::from(c - b'0');
    }
    Some(n)
}

/// Days since 1970-01-01 → (year, month, day). Hand-rolled Gregorian.
/// Shifts epoch to 1 Mar 0000 for easier leap-year arithmetic.
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
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

/// (year, month, day) → days since 1970-01-01. Inverse of `days_to_ymd`.
/// Returns `None` for impossible inputs (zero / out-of-range month or day,
/// or any combination that lands before 1970-01-01).
fn ymd_to_days(y: u64, m: u64, d: u64) -> Option<u64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y / 400;
    let yoe = y % 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe;
    days.checked_sub(719468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_secs_known_date() {
        assert_eq!(unix_secs_to_rfc3339(1779494400), "2026-05-23T00:00:00Z");
        assert_eq!(unix_secs_to_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn parse_then_format_roundtrips() {
        let ts = "2026-05-23T07:30:45Z";
        let secs = parse_rfc3339_secs(ts).expect("parse");
        assert_eq!(unix_secs_to_rfc3339(secs), ts);
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(parse_rfc3339_secs("not a date").is_none());
        assert!(parse_rfc3339_secs("2026-05-23T07:30:45").is_none()); // missing Z
        assert!(parse_rfc3339_secs("2026-13-01T00:00:00Z").is_none()); // bad month
        assert!(parse_rfc3339_secs("2026-05-32T00:00:00Z").is_none()); // bad day
    }

    /// A jsonl line can hold any UTF-8. These pass the length / `T` / `Z`
    /// gate but put a multi-byte char across a field boundary, which the
    /// old `&s[0..4]` slicing turned into a panic — in a reader whose
    /// contract is to return `None` for anything malformed.
    #[test]
    fn parse_rejects_multibyte_chars_without_panicking() {
        // 'é' is 2 bytes, occupying byte 3 and 4 — straddling the year field's
        // end. 20 bytes total, 'T' at 10, 'Z' at 19.
        let straddles_year = "202é01-01T00:00:00Z";
        assert_eq!(straddles_year.len(), 20);
        assert_eq!(straddles_year.as_bytes()[10], b'T');
        assert_eq!(straddles_year.as_bytes()[19], b'Z');
        assert!(parse_rfc3339_secs(straddles_year).is_none());

        // 'ü' straddles the hour field's start (bytes 11-12). 21 bytes, so
        // it clears the length gate too.
        let straddles_hour = "2026-05-23Tü:30:45Z0";
        assert_eq!(straddles_hour.as_bytes()[10], b'T');
        assert_eq!(straddles_hour.as_bytes()[19], b'Z');
        assert!(parse_rfc3339_secs(straddles_hour).is_none());
    }
}
