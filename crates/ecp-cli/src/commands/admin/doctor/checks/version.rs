//! Version freshness. Report-only: compares the compiled-in version against the
//! latest *published GitHub Release* and, if newer, suggests an upgrade command
//! matching the install channel.
//!
//! Why a Release, not a git tag: the release pipeline pushes the `v*` tag first,
//! then builds binaries and publishes npm/PyPI packages minutes later. Querying
//! `git ls-remote --tags` would announce a version whose packages aren't on the
//! registries yet — a `cargo install --tag` can race ahead, but `npx`/`uvx`
//! users would be told to upgrade to something they can't install. The
//! `releases/latest` API only returns a non-draft, non-prerelease release, which
//! the pipeline creates after the build — so by the time it reports, the
//! download assets exist (packages still trail by a couple minutes; close
//! enough, and far better than the tag).
//!
//! Falls back to the git-tag query when `curl` is unavailable. Network failure —
//! including a restricted-network sandbox where the connect blocks instead of
//! failing fast — degrades to a Warn rather than hanging or failing the run;
//! never prompts or updates.

use std::time::Duration;

use crate::commands::admin::doctor::checks::install_source::InstallSource;
use crate::commands::admin::doctor::CheckResult;
use crate::git::safe_exec;

const REPO_URL: &str = "https://github.com/coseto6125/egent-code-plexus";
const RELEASES_LATEST_API: &str =
    "https://api.github.com/repos/coseto6125/egent-code-plexus/releases/latest";

/// Hard ceiling on the remote query. A sandboxed network can leave `git
/// ls-remote` blocked in poll() well past any HTTP-layer timeout, so the
/// subprocess is killed outright at this bound.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn check() -> CheckResult {
    let local = env!("CARGO_PKG_VERSION");
    let local_parsed = match parse_semver(local) {
        Some(v) => v,
        None => return CheckResult::ok("version", format!("local v{local} (no comparison)")),
    };

    // Fast path: in a network-restricted agent sandbox (Codex / Gemini) the
    // remote connect would block until the timeout backstop fires, turning an
    // instant report into a multi-second stall. Skip the query and report
    // offline immediately.
    if safe_exec::sandbox_network_restricted() {
        return CheckResult::ok("version", format!("local v{local} (offline: sandbox)"));
    }

    match latest_published_version() {
        Some(latest) if latest > local_parsed => CheckResult::warn(
            "version",
            format!(
                "v{}.{}.{} available (local v{local})",
                latest.0, latest.1, latest.2
            ),
        )
        .with_remediation(InstallSource::detect().upgrade_hint()),
        Some(_) => CheckResult::ok("version", format!("up to date (v{local})")),
        None => CheckResult::warn("version", format!("local v{local}; could not reach remote")),
    }
}

/// Latest installable version: the `tag_name` of the newest published GitHub
/// Release (`curl` → `releases/latest`), falling back to the highest `git
/// ls-remote` tag when curl is missing. Both run under the kill-timeout so a
/// blocked connect can't hang the check. `None` on any failure.
fn latest_published_version() -> Option<(u32, u32, u32)> {
    if let Some(v) = latest_release_via_curl() {
        return Some(v);
    }
    latest_tag_via_git()
}

/// `releases/latest` returns only a non-draft, non-prerelease release, so its
/// `tag_name` won't appear until the pipeline has built and published assets.
fn latest_release_via_curl() -> Option<(u32, u32, u32)> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args([
        "-sS",
        "--max-time",
        "5",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "User-Agent: ecp-doctor",
        RELEASES_LATEST_API,
    ]);
    let out = safe_exec::output_with_timeout(cmd, REMOTE_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    parse_release_tag(&String::from_utf8_lossy(&out.stdout))
}

/// Extract and parse `tag_name` from a `releases/latest` JSON body. Pure, so the
/// parse is unit-tested without a network call.
fn parse_release_tag(body: &str) -> Option<(u32, u32, u32)> {
    let tag = serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("tag_name")?
        .as_str()?
        .to_string();
    parse_semver(&tag)
}

/// Backstop for when `curl` isn't installed. Reads tags directly; note this can
/// see a tag whose Release/packages aren't published yet (the race this module
/// otherwise avoids), but a stale-by-minutes hint beats no hint.
fn latest_tag_via_git() -> Option<(u32, u32, u32)> {
    let mut cmd = safe_exec::git();
    // GIT_HTTP_LOW_SPEED_* makes git itself abort a stalled transfer; the
    // output_with_timeout kill is the backstop for a connect that never even
    // reaches the transfer phase (sandbox drops the SYN).
    cmd.env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_HTTP_LOW_SPEED_TIME", "5")
        .args(["ls-remote", "--tags", "--refs", REPO_URL]);
    let out = safe_exec::output_with_timeout(cmd, REMOTE_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    parse_latest_tag(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `X.Y.Z` (leading `v` tolerated) into a comparable tuple.
fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    // Patch may carry a suffix (e.g. "2+sha", "2-rc1") — take the leading digits.
    let patch_raw = parts.next()?;
    let patch = patch_raw
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

/// Highest semver tag from `git ls-remote --tags` output. Each line is
/// `<sha>\trefs/tags/<tag>`; non-semver tags are skipped.
fn parse_latest_tag(ls_remote: &str) -> Option<(u32, u32, u32)> {
    ls_remote
        .lines()
        .filter_map(|line| line.rsplit("refs/tags/").next())
        .filter_map(parse_semver)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_semver_tolerates_v_prefix_and_suffix() {
        assert_eq!(parse_semver("v0.4.2"), Some((0, 4, 2)));
        assert_eq!(parse_semver("0.4.2"), Some((0, 4, 2)));
        assert_eq!(parse_semver("0.4.2+abc123"), Some((0, 4, 2)));
        assert_eq!(parse_semver("1.10.0"), Some((1, 10, 0)));
        assert_eq!(parse_semver("not-a-version"), None);
    }

    #[test]
    fn parse_latest_tag_picks_highest_semver() {
        let out = "\
abc123\trefs/tags/v0.3.0
def456\trefs/tags/v0.4.2
789aaa\trefs/tags/v0.4.10
000bbb\trefs/tags/some-non-semver-tag";
        // 0.4.10 > 0.4.2 numerically (not lexically).
        assert_eq!(parse_latest_tag(out), Some((0, 4, 10)));
    }

    #[test]
    fn parse_latest_tag_empty_is_none() {
        assert_eq!(parse_latest_tag(""), None);
    }

    #[test]
    fn parse_release_tag_reads_tag_name() {
        // Trimmed shape of the releases/latest payload.
        let body = r#"{"tag_name":"v0.5.1","draft":false,"prerelease":false}"#;
        assert_eq!(parse_release_tag(body), Some((0, 5, 1)));
    }

    #[test]
    fn parse_release_tag_handles_missing_or_malformed() {
        assert_eq!(parse_release_tag(r#"{"message":"Not Found"}"#), None);
        assert_eq!(parse_release_tag("not json"), None);
        assert_eq!(parse_release_tag(r#"{"tag_name":"nightly"}"#), None);
    }
}
