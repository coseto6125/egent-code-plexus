//! Version freshness. Report-only: queries the latest published tag via
//! `git ls-remote --tags` (no network-client dependency, reuses the hardened
//! git wrapper) and compares against the compiled-in version. Network failure
//! degrades to a Warn rather than failing the run; never prompts or updates.

use crate::commands::admin::doctor::CheckResult;
use crate::git::safe_exec;

const REPO_URL: &str = "https://github.com/coseto6125/egent-code-plexus";
const INSTALL_CMD: &str =
    "cargo install --git https://github.com/coseto6125/egent-code-plexus egent-code-plexus --bin ecp --locked";

pub(crate) fn check() -> CheckResult {
    let local = env!("CARGO_PKG_VERSION");
    let local_parsed = match parse_semver(local) {
        Some(v) => v,
        None => return CheckResult::ok("version", format!("local v{local} (no comparison)")),
    };

    let output = safe_exec::git()
        .args(["ls-remote", "--tags", "--refs", REPO_URL])
        .output();
    let stdout = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            return CheckResult::warn("version", format!("local v{local}; could not reach remote"))
        }
    };

    match parse_latest_tag(&stdout) {
        Some(latest) if latest > local_parsed => CheckResult::warn(
            "version",
            format!(
                "v{}.{}.{} available (local v{local})",
                latest.0, latest.1, latest.2
            ),
        )
        .with_remediation(INSTALL_CMD),
        Some(_) => CheckResult::ok("version", format!("up to date (v{local})")),
        None => CheckResult::warn("version", format!("local v{local}; no remote tags found")),
    }
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
}
