//! `ecp admin check-update` — hidden, background-only update probe.
//!
//! Spawned detached by the session_start hook (never invoked by a user or the
//! LLM). It decides *whether* to hit the network based on a throttle file, so
//! the network call happens at most once a day, and at most once per 8h after a
//! failure. Reuses the doctor's version logic (`latest_published_version`) so
//! there is one network/parse implementation, not two.
//!
//! Throttle (`<home_ecp>/.update-check.json`):
//!   - succeeded today                  → skip (one check per day)
//!   - last attempt failed < 8h ago     → skip (back off)
//!   - otherwise                         → query
//!
//! On a successful query with a newer remote version, writes
//! `<home_ecp>/.update-available` (consumed once by UserPromptSubmit). Network
//! failure is silent: it only stamps `last_attempt_epoch` so the 8h backoff
//! applies, and never writes a notification or surfaces an error.
//!
//! Independently of the throttle, every run compares the running version with
//! the one recorded on the last run. A change that `ecp update` did not make
//! is a channel upgrade (npm / uv / pip / brew / cargo), and the notice tells
//! the user that path is removed in 0.15. This is the one place a channel
//! upgrade can be seen: the package managers never call back into ecp.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use ecp_core::registry::{atomic_write_json, resolve_home_ecp, FileLock};
use ecp_core::EcpError;
use serde::{Deserialize, Serialize};

use crate::commands::admin::doctor::checks::install_source::{CHANNELS, CHANNEL_SUNSET};
use crate::commands::admin::doctor::checks::version::{latest_published_version, parse_semver};
use crate::git::safe_exec;

const DAY_SECS: u64 = 86_400;
const FAIL_BACKOFF_SECS: u64 = 8 * 3_600;

/// Persisted throttle state. Missing / unreadable file is treated as "never
/// checked" — the probe runs. Success and failure are tracked separately so the
/// two throttle rules (daily cap, 8h failure backoff) don't have to be inferred
/// from one ambiguous timestamp.
#[derive(Default, Serialize, Deserialize)]
struct CheckState {
    /// Version of the binary that last ran the probe, or that `ecp update`
    /// last installed. Absent in files written before `ecp update` existed,
    /// which reads as "nothing to compare yet".
    #[serde(default)]
    seen_version: String,
    /// Day bucket (`epoch / 86400`) of the last *successful* network query.
    last_success_day: u64,
    /// Unix epoch (secs) of the last *failed* attempt. `0` = no pending
    /// failure (cleared on success). Drives the 8h backoff.
    last_failure_epoch: u64,
    /// Latest version string seen from the remote, for the notification text.
    latest_version: String,
}

pub fn run() -> Result<(), EcpError> {
    let home_ecp = resolve_home_ecp();
    let state_path = home_ecp.join(".update-check.json");
    let now = now_epoch();
    let mut state = read_state(&state_path);
    let local = env!("CARGO_PKG_VERSION");
    let mut notices = Vec::new();

    if let Some(notice) = channel_update_notice(&state, local) {
        notices.push(notice);
    }
    state.seen_version = local.to_string();

    if should_query(&state, now) {
        // A restricted-network sandbox would block until the timeout backstop;
        // treat it as a failure so the 8h backoff applies. Network failure is
        // silent too: it only moves the backoff clock.
        let latest = (!safe_exec::sandbox_network_restricted())
            .then(latest_published_version)
            .flatten();
        match latest {
            Some(latest) => {
                let latest_str = format!("{}.{}.{}", latest.0, latest.1, latest.2);
                if parse_semver(local).is_some_and(|l| latest > l) {
                    notices.push(available_notice(&latest_str, local));
                }
                // Success clears any pending failure backoff.
                state.last_success_day = now / DAY_SECS;
                state.last_failure_epoch = 0;
                state.latest_version = latest_str;
            }
            None => state.last_failure_epoch = now,
        }
    }
    write_state(&state_path, &state);
    write_notification(&home_ecp, &notices);
    Ok(())
}

/// Called by `ecp update` from the outgoing binary, so the incoming binary's
/// first probe sees its own version as expected. Takes the probe's flock: a
/// probe mid-query would otherwise write back the version it read before the
/// swap and the next session would report the update as a channel upgrade.
pub(crate) fn record_self_update(version: &str) {
    let home_ecp = resolve_home_ecp();
    let _lock = FileLock::acquire_exclusive(&home_ecp.join(".update-check.lock"));
    record_self_update_at(&home_ecp.join(".update-check.json"), version);
}

/// Only the version field changes; the throttle fields must survive so the
/// next session does not re-query the network.
fn record_self_update_at(state_path: &Path, version: &str) {
    let mut state = read_state(state_path);
    state.seen_version = version.to_string();
    write_state(state_path, &state);
}

/// Text every "newer version available" line carries, and no other notice does.
const AVAILABLE_MARK: &str = " is available (you have v";

/// Drop the pending "newer version available" line once it no longer applies.
/// Other lines (a channel-upgrade notice) fire once and must survive.
pub(crate) fn clear_available_notice() {
    clear_available_notice_at(&resolve_home_ecp().join(".update-available"));
}

fn clear_available_notice_at(marker: &Path) {
    let Ok(body) = std::fs::read_to_string(marker) else {
        return;
    };
    let kept: Vec<&str> = body
        .lines()
        .filter(|line| !line.contains(AVAILABLE_MARK))
        .collect();
    if kept.is_empty() {
        let _ = std::fs::remove_file(marker);
    } else {
        let _ = std::fs::write(marker, kept.join("\n"));
    }
}

/// The running version differs from the last recorded one, which `ecp update`
/// would have moved along with the binary. First run (nothing recorded) is
/// silent.
fn channel_update_notice(state: &CheckState, local: &str) -> Option<String> {
    if state.seen_version.is_empty() || state.seen_version == local {
        return None;
    }
    Some(format!(
        "ecp changed from v{} to v{local} outside `ecp update`. Upgrading through {CHANNELS} is removed in {CHANNEL_SUNSET}; run `ecp update` from now on.",
        state.seen_version
    ))
}

fn available_notice(latest: &str, local: &str) -> String {
    format!(
        "ecp v{latest}{AVAILABLE_MARK}{local}). Run `ecp update`. Upgrading through {CHANNELS} still works until {CHANNEL_SUNSET}, when `ecp update` becomes the only path."
    )
}

/// Query when we haven't succeeded today AND we're past the 8h failure backoff.
/// The two rules are independent: a same-day success blocks re-query regardless
/// of failures; a recent failure blocks re-query even on a fresh day.
fn should_query(state: &CheckState, now: u64) -> bool {
    let succeeded_today = state.last_success_day == now / DAY_SECS;
    let in_backoff = state.last_failure_epoch != 0
        && now.saturating_sub(state.last_failure_epoch) < FAIL_BACKOFF_SECS;
    !succeeded_today && !in_backoff
}

/// Append each notice the marker does not already hold. The marker is drained
/// on the next prompt; a session that never submits one must not pile up the
/// same daily line, and a newer "available" line replaces an older one.
fn write_notification(home_ecp: &Path, notices: &[String]) {
    if notices.is_empty() {
        return;
    }
    let marker = home_ecp.join(".update-available");
    let existing = std::fs::read_to_string(&marker).unwrap_or_default();
    let fresh: Vec<&str> = notices
        .iter()
        .map(String::as_str)
        .filter(|n| !existing.contains(n))
        .collect();
    if fresh.is_empty() {
        return;
    }
    let supersedes_available = fresh.iter().any(|n| n.contains(AVAILABLE_MARK));
    let body: Vec<&str> = existing
        .lines()
        .filter(|line| !line.is_empty() && !(supersedes_available && line.contains(AVAILABLE_MARK)))
        .chain(fresh)
        .collect();
    let _ = std::fs::write(marker, body.join("\n"));
}

fn read_state(path: &Path) -> CheckState {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_state(path: &Path, state: &CheckState) {
    let _ = atomic_write_json(path, state);
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(success_day: u64, failure_epoch: u64) -> CheckState {
        CheckState {
            last_success_day: success_day,
            last_failure_epoch: failure_epoch,
            ..Default::default()
        }
    }

    fn seen(version: &str) -> CheckState {
        CheckState {
            seen_version: version.into(),
            ..Default::default()
        }
    }

    #[test]
    fn test_channel_update_notice_first_run_is_silent() {
        assert_eq!(channel_update_notice(&seen(""), "0.13.3"), None);
    }

    #[test]
    fn test_channel_update_notice_same_version_is_silent() {
        assert_eq!(channel_update_notice(&seen("0.13.3"), "0.13.3"), None);
    }

    #[test]
    fn test_channel_update_notice_rollback_through_a_channel_is_reported() {
        // 0.13.3 installed by `ecp update`, 0.13.4 via brew (reported), then
        // `brew install ecp@0.13.3`: the walk back is a channel change too.
        assert!(channel_update_notice(&seen("0.13.4"), "0.13.3").is_some());
    }

    #[test]
    fn test_channel_update_notice_foreign_change_names_both_versions_and_015() {
        let notice = channel_update_notice(&seen("0.13.2"), "0.13.4").unwrap();
        // Contract: the reader learns the old version, the new one, that
        // channel upgrades end in 0.15, and the command to use instead.
        assert!(notice.contains("v0.13.2"), "{notice}");
        assert!(notice.contains("v0.13.4"), "{notice}");
        assert!(notice.contains(CHANNEL_SUNSET), "{notice}");
        assert!(notice.contains("`ecp update`"), "{notice}");
    }

    #[test]
    fn test_read_state_from_pre_update_file_keeps_throttle_and_reads_empty_versions() {
        // Literal shape `ecp admin check-update` wrote before the two version
        // fields existed (0.6.x through 0.13.2).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".update-check.json");
        std::fs::write(
            &path,
            r#"{"last_success_day":20705,"last_failure_epoch":0,"latest_version":"0.13.2"}"#,
        )
        .unwrap();

        let state = read_state(&path);

        assert_eq!(state.last_success_day, 20705);
        assert_eq!(state.latest_version, "0.13.2");
        assert_eq!(state.seen_version, "");
        assert_eq!(channel_update_notice(&state, "0.13.3"), None);
    }

    #[test]
    fn test_record_self_update_at_keeps_throttle_fields_and_sets_seen_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".update-check.json");
        std::fs::write(
            &path,
            r#"{"last_success_day":20705,"last_failure_epoch":7,"latest_version":"0.13.3","seen_version":"0.13.2"}"#,
        )
        .unwrap();

        record_self_update_at(&path, "0.13.3");

        let state = read_state(&path);
        assert_eq!(state.last_success_day, 20705);
        assert_eq!(state.last_failure_epoch, 7);
        assert_eq!(state.latest_version, "0.13.3");
        assert_eq!(state.seen_version, "0.13.3");
        // The incoming binary's first probe must not report its own arrival.
        assert_eq!(channel_update_notice(&state, "0.13.3"), None);
    }

    #[test]
    fn test_clear_available_notice_at_keeps_channel_line_and_removes_empty_marker() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(".update-available");
        let channel = channel_update_notice(&seen("0.13.2"), "0.13.3").unwrap();
        write_notification(
            dir.path(),
            &[available_notice("0.13.4", "0.13.3"), channel.clone()],
        );

        clear_available_notice_at(&marker);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), channel);

        clear_available_notice_at(&marker);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), channel);

        std::fs::write(&marker, available_notice("0.13.4", "0.13.3")).unwrap();
        clear_available_notice_at(&marker);
        assert!(!marker.exists());
        clear_available_notice_at(&marker);
    }

    #[test]
    fn test_write_notification_appends_only_lines_not_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(".update-available");
        write_notification(dir.path(), &["first".into()]);
        write_notification(dir.path(), &["first".into(), "second".into()]);
        write_notification(dir.path(), &[]);

        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "first\nsecond");
        assert!(!dir.path().join("missing").exists());
    }

    #[test]
    fn test_write_notification_newer_available_line_replaces_the_older_one() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(".update-available");
        let channel = channel_update_notice(&seen("0.13.2"), "0.13.3").unwrap();
        write_notification(
            dir.path(),
            &[channel.clone(), available_notice("0.13.4", "0.13.3")],
        );
        write_notification(dir.path(), &[available_notice("0.13.5", "0.13.3")]);

        let body = std::fs::read_to_string(&marker).unwrap();
        assert_eq!(
            body,
            format!("{channel}\n{}", available_notice("0.13.5", "0.13.3"))
        );
    }

    #[test]
    fn first_ever_check_queries() {
        // Never checked (all zero) → query.
        assert!(should_query(&state(0, 0), 10 * DAY_SECS));
    }

    #[test]
    fn same_day_success_skips() {
        let now = 10 * DAY_SECS + 500;
        // Succeeded today, no failure pending.
        assert!(!should_query(&state(10, 0), now));
    }

    #[test]
    fn same_day_success_skips_even_with_old_failure() {
        let now = 10 * DAY_SECS + 500;
        // Daily cap wins regardless of an ancient failure timestamp.
        assert!(!should_query(&state(10, now - 100), now));
    }

    #[test]
    fn new_day_after_success_queries() {
        let now = 11 * DAY_SECS + 500;
        // Last success was yesterday (day 10), no pending failure.
        assert!(should_query(&state(10, 0), now));
    }

    #[test]
    fn recent_failure_backs_off_even_on_new_day() {
        // Failed 1h ago on a fresh day → still inside 8h backoff.
        let now = 12 * DAY_SECS + 3_600;
        assert!(!should_query(&state(10, now - 3_600), now));
    }

    #[test]
    fn failure_past_8h_retries() {
        // Failed 9h ago → past the 8h backoff → retry.
        let now = 12 * DAY_SECS + 10 * 3_600;
        assert!(should_query(&state(10, now - 9 * 3_600), now));
    }
}
