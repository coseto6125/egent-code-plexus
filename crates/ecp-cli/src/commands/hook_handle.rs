//! `ecp hook-handle <stage>`: invoked by the reference-transaction hook.

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct HookHandleArgs {
    /// Reference-transaction stage: prepared / committed / aborted.
    pub stage: String,
}

/// The watcher this used to spawn on a branch delete ran commands that no
/// longer exist (`rename-branch`, `prune`), so the handler only keeps
/// already-installed `reference-transaction` hooks exiting 0. It still drains
/// stdin: exiting first would hand the writer (git) a broken pipe.
pub fn run(_args: HookHandleArgs) -> Result<(), ecp_core::EcpError> {
    std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink())?;
    Ok(())
}
