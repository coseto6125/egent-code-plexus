//! A git `reference-transaction` hook that an older `ecp admin install-hook`
//! wrote. `ecp hook-handle` now only drains stdin, so the hook spawns a
//! process on every ref update for nothing, and one that points at a removed
//! binary fails the `prepared` stage, which aborts the ref update.

use crate::commands::admin::doctor::CheckResult;
use crate::commands::uninstall::{
    is_ecp_reference_transaction_hook, reference_transaction_hook_paths,
};
use std::path::Path;

const NAME: &str = "config:git-hook";
const REMOVE: &str = "ecp admin uninstall-hook";

/// Reports nothing outside a repository or when no ecp hook is present.
pub(crate) fn check() -> Vec<CheckResult> {
    std::env::current_dir()
        .map(|cwd| check_in(&cwd))
        .unwrap_or_default()
}

fn check_in(dir: &Path) -> Vec<CheckResult> {
    reference_transaction_hook_paths(dir)
        .unwrap_or_default()
        .iter()
        .filter_map(|hook| finding(hook))
        .collect()
}

fn finding(hook: &Path) -> Option<CheckResult> {
    let body = match std::fs::read(hook) {
        Ok(body) => body,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(CheckResult::warn(
                NAME,
                format!(
                    "cannot read {} ({e}), so cannot tell whether an older ecp installed it",
                    hook.display()
                ),
            ))
        }
    };
    is_ecp_reference_transaction_hook(&String::from_utf8_lossy(&body)).then(|| {
        CheckResult::warn(
            NAME,
            format!(
                "{} is a retired ecp hook: `ecp hook-handle` no longer does anything",
                hook.display()
            ),
        )
        .with_remediation(REMOVE)
    })
}
