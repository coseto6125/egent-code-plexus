//! Graph index freshness, via the same staleness logic agent commands use.

use crate::auto_ensure::{ensure_index, EnsureResult};
use crate::commands::admin::doctor::CheckResult;
use crate::commands::admin::index::{self, IndexArgs};
use crate::graph_path;
use std::path::PathBuf;

pub(crate) fn check(fix: bool) -> CheckResult {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return CheckResult::fail("index", format!("cannot read cwd: {e}")),
    };
    let graph = graph_path::resolve(&PathBuf::from(".ecp/graph.bin"), &cwd);

    let remediation = "ecp admin index --repo .";
    let mut result = match ensure_index(&graph, &cwd) {
        Ok(EnsureResult::Ready) => return CheckResult::ok("index", "graph is fresh"),
        Ok(EnsureResult::Stale { age_seconds, .. }) => CheckResult::warn(
            "index",
            format!("stale — graph built {age_seconds}s before latest source change"),
        )
        .with_remediation(remediation),
        Ok(EnsureResult::Missing) => {
            CheckResult::fail("index", "no graph index found").with_remediation(remediation)
        }
        Err(e) => return CheckResult::fail("index", format!("freshness probe failed: {e}")),
    };

    if fix {
        let args = IndexArgs {
            repo: cwd.to_string_lossy().into_owned(),
            force: false,
            dump_resolver: None,
            quiet: true,
        };
        result.fix_applied = Some(index::run(args).is_ok());
    }
    result
}
