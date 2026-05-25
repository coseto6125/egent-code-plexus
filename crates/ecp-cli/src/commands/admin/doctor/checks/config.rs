//! Config / path sanity: ECP_HOME resolves and is writable; the registry
//! dir exists; the Claude skills parent dir exists.

use crate::commands::admin::doctor::CheckResult;
use crate::git::safe_exec;
use ecp_core::registry::resolve_home_ecp;
use std::path::{Path, PathBuf};

pub(crate) fn check() -> Vec<CheckResult> {
    let home_ecp = resolve_home_ecp();
    let mut out = vec![git_check(), ecp_home_check(&home_ecp)];

    let claude_skills = claude_home().join("skills");
    out.push(if claude_skills.is_dir() {
        CheckResult::ok(
            "config:claude-dir",
            format!("{} exists", claude_skills.display()),
        )
    } else {
        CheckResult::warn(
            "config:claude-dir",
            format!(
                "{} missing — no skills installed yet",
                claude_skills.display()
            ),
        )
        .with_remediation("ecp admin claude install skills all")
    });

    out
}

/// git is a hard prerequisite — index freshness, review diffs, and the version
/// check all shell out to it. Absent git is a Fail (not Warn): core features
/// silently degrade without it.
fn git_check() -> CheckResult {
    match safe_exec::git().arg("--version").output() {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout);
            CheckResult::ok("config:git", v.trim().to_string())
        }
        _ => CheckResult::fail("config:git", "git not found on PATH")
            .with_remediation("install git (https://git-scm.com/downloads)"),
    }
}

/// ECP_HOME must resolve to an existing, writable dir. A write-probe failure
/// is a Warn (ecp falls back to a temp dir, degraded but functional), not Fail.
fn ecp_home_check(home_ecp: &Path) -> CheckResult {
    if !home_ecp.is_dir() {
        return CheckResult::warn(
            "config:ecp-home",
            format!(
                "{} does not exist yet (created on first index)",
                home_ecp.display()
            ),
        );
    }
    let probe = home_ecp.join(".doctor-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            CheckResult::ok(
                "config:ecp-home",
                format!("{} writable", home_ecp.display()),
            )
        }
        Err(e) => CheckResult::warn(
            "config:ecp-home",
            format!(
                "{} not writable ({e}) — ecp will fall back to a temp dir",
                home_ecp.display()
            ),
        ),
    }
}

fn claude_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
}
