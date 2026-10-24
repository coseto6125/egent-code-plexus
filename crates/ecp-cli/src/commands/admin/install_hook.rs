//! `ecp admin install-hook --claude-code`: installs entries into Claude Code's
//! settings.json.
//!
//! Without `--claude-code` it refuses. It used to install a git
//! `reference-transaction` hook, and `ecp hook-handle` now only drains stdin,
//! so that hook would cost a process on every ref update and do nothing.

use crate::commands::admin::claude_code;
use clap::Args;
use std::path::PathBuf;

const RETIRED_GIT_HOOK: &str = "the git reference-transaction hook is retired: \
`ecp hook-handle` no longer does anything, so nothing was written. To remove a hook an \
older ecp installed, run `ecp admin uninstall-hook` inside the repository. For Claude \
Code hooks, run `ecp admin install-hook --claude-code`.";

#[derive(Args, Debug, Clone)]
pub struct InstallHookArgs {
    /// Ignored: the git hook it applied to is retired.
    #[arg(long, default_value_t = false, hide = true)]
    pub force: bool,

    /// Ignored: the git hook it applied to is retired.
    #[arg(long, default_value_t = false, hide = true)]
    pub no_chain: bool,

    /// Install Claude Code settings.json hook entries. Required: the git hook
    /// this command installed without it is retired.
    #[arg(long, default_value_t = false)]
    pub claude_code: bool,

    /// CSV of events when --claude-code is set (session-start, user-prompt-submit,
    /// pre-tool-use, post-tool-use). Omit for an interactive multi-select.
    #[arg(long)]
    pub events: Option<String>,

    /// Override Claude Code settings.json path (default `~/.claude/settings.json`).
    #[arg(long, hide = true)]
    pub settings_path: Option<PathBuf>,
}

pub fn run(args: InstallHookArgs) -> Result<(), ecp_core::EcpError> {
    if !args.claude_code {
        return Err(ecp_core::EcpError::InvalidArgument(RETIRED_GIT_HOOK.into()));
    }
    claude_code::run_install_claude_code(args.events.as_deref(), args.settings_path.as_deref())
}
