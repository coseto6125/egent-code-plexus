//! Host-integration consistency across all known Claude-Code / agent hosts.
//! Scripted hosts (claude/gemini mcp+native, codex mcp) auto-fix under `--fix`;
//! the rest are report-only (stubs pending implementation, or interactive-only).

use crate::admin::host_integration::{mcp, native};
use crate::admin::status::HostStatus;
use crate::commands::admin::doctor::CheckResult;
use ecp_core::EcpError;

/// A host's fix entry point, when one can run non-interactively.
type FixFn = fn() -> Result<(), EcpError>;

pub(crate) fn check(fix: bool) -> Vec<CheckResult> {
    let codex_mcp_fix: FixFn = || mcp::codex::run_install().map(|_| ());
    let hosts: Vec<(&str, HostStatus, Option<FixFn>)> = vec![
        (
            "host:claude-code-mcp",
            mcp::claude_code::status(),
            Some(mcp::claude_code::install_scripted as FixFn),
        ),
        (
            "host:gemini-mcp",
            mcp::gemini::status(),
            Some(mcp::gemini::install_scripted as FixFn),
        ),
        (
            "host:gemini-native",
            native::gemini::status(),
            Some(native::gemini::install_scripted as FixFn),
        ),
        ("host:codex-mcp", mcp::codex::status(), Some(codex_mcp_fix)),
        ("host:codex-native", native::codex::status(), None),
        ("host:cursor-mcp", mcp::cursor::status(), None),
        ("host:copilot-mcp", mcp::copilot::status(), None),
        ("host:windsurf-mcp", mcp::windsurf::status(), None),
        ("host:cline-roo-mcp", mcp::cline_roo::status(), None),
        ("host:generic-mcp", mcp::generic::status(), None),
    ];

    hosts
        .into_iter()
        .map(|(name, status, fix_fn)| map_host(name, status, fix, fix_fn))
        .collect()
}

/// Installed → Ok. Outdated → Warn (+ auto-fix when a scripted fix exists and
/// `--fix` is set). Missing → Ok "optional" (a missing optional host isn't a
/// problem; it just isn't wired up).
fn map_host(name: &str, status: HostStatus, fix: bool, fix_fn: Option<FixFn>) -> CheckResult {
    match status {
        HostStatus::Installed { detail } => CheckResult::ok(name, format!("installed ({detail})")),
        HostStatus::Missing => CheckResult::ok(name, "not integrated (optional)"),
        HostStatus::Outdated { reason } => {
            // All host checks share the `host` selector, so the single-target
            // fix command is always `ecp admin doctor host --fix`.
            let mut r = CheckResult::warn(name, format!("outdated — {reason}"))
                .with_remediation("ecp admin doctor host --fix");
            if fix {
                if let Some(f) = fix_fn {
                    r.fix_applied = Some(f().is_ok());
                }
            }
            r
        }
    }
}
