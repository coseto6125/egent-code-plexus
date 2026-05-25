//! `ecp admin doctor` — environment health check.
//!
//! Aggregates independent checks (installed skills freshness, graph index
//! freshness, host-integration consistency, config/path sanity) into one
//! report. Default is read-only; `--fix` reruns the fixable remediations
//! (skill reinstall, index rebuild) in place.

mod checks;

use crate::output::{emit, OutputFormat};
use clap::{Args, ValueEnum};
use ecp_core::EcpError;
use serde::Serialize;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckTarget {
    Skills,
    Index,
    Host,
    Config,
    Registry,
    Version,
}

#[derive(Args, Debug, Clone)]
pub struct DoctorArgs {
    /// Run only this check (skills / index / host / config / registry /
    /// version). Omit to run all.
    #[arg(value_enum)]
    pub check: Option<CheckTarget>,
    /// Apply fixable remediations for the selected check(s): reinstall stale
    /// skills, rebuild a stale index, remove orphan index dirs, reinstall
    /// scripted host integrations. Config / version findings are report-only.
    #[arg(long)]
    pub fix: bool,
    /// Output format: text (default) / json / toon.
    #[arg(long)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// Set only when `--fix` ran for this check: whether the fix succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_applied: Option<bool>,
}

impl CheckResult {
    pub fn ok(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Ok,
            message: message.into(),
            remediation: None,
            fix_applied: None,
        }
    }

    pub fn warn(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warn,
            message: message.into(),
            remediation: None,
            fix_applied: None,
        }
    }

    pub fn fail(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Fail,
            message: message.into(),
            remediation: None,
            fix_applied: None,
        }
    }

    pub fn with_remediation(mut self, hint: impl Into<String>) -> Self {
        self.remediation = Some(hint.into());
        self
    }
}

pub fn run(args: DoctorArgs) -> Result<(), EcpError> {
    let fix = args.fix;
    // `check` selects a single check; None runs all. A match on the target
    // keeps each check's fix wired only when that check is in scope.
    let want = |t: CheckTarget| args.check.is_none() || args.check == Some(t);

    let mut results = Vec::new();
    if want(CheckTarget::Skills) {
        results.extend(checks::skills::check(fix));
    }
    if want(CheckTarget::Index) {
        results.push(checks::index::check(fix));
    }
    if want(CheckTarget::Host) {
        results.extend(checks::host::check(fix));
    }
    if want(CheckTarget::Config) {
        results.extend(checks::config::check());
    }
    if want(CheckTarget::Registry) {
        results.extend(checks::registry::check(fix));
    }
    if want(CheckTarget::Version) {
        results.push(checks::version::check());
    }

    let fail = results
        .iter()
        .filter(|r| r.status == CheckStatus::Fail)
        .count();

    let format = OutputFormat::parse(args.format.as_deref());
    match format {
        OutputFormat::Json | OutputFormat::Toon => {
            emit(&serde_json::json!({ "checks": results }), format)?;
        }
        _ => print_text(&results),
    }

    if fail > 0 {
        return Err(EcpError::Output(format!("doctor: {fail} check(s) failed")));
    }
    Ok(())
}

fn print_text(results: &[CheckResult]) {
    let mut warn = 0usize;
    let mut fail = 0usize;
    for r in results {
        let tag = match r.status {
            CheckStatus::Ok => "ok  ",
            CheckStatus::Warn => {
                warn += 1;
                "warn"
            }
            CheckStatus::Fail => {
                fail += 1;
                "fail"
            }
        };
        println!("[{tag}] {}: {}", r.name, r.message);
        if let Some(hint) = &r.remediation {
            match r.fix_applied {
                Some(true) => println!("       fixed: ran `{hint}`"),
                Some(false) => println!("       fix failed — run manually: `{hint}`"),
                None => println!("       hint: {hint}"),
            }
        }
    }
    println!(
        "\n{} checks · {} ok · {} warn · {} fail",
        results.len(),
        results.len() - warn - fail,
        warn,
        fail
    );
}
