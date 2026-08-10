//! `ecp hook <event> --claude-code` — Claude Code hook entry point.
//!
//! Reads a JSON envelope on stdin, dispatches to the per-event handler,
//! and emits a `{"hookSpecificOutput": ...}` JSON response on stdout
//! (empty stdout means no-op — Claude Code treats that as "nothing to
//! add to the conversation").
//!
//! Per-event logic lives in sibling modules so each handler is a
//! self-contained file. Shared utilities (stdin parse, response emit,
//! marker paths, shell-quote stripping) live in `common`.

pub mod agent_dispatch;
pub mod common;
pub mod post_tool_use;
pub mod pre_tool_use;
pub mod session_start;
pub mod user_prompt_submit;

use clap::{Args, ValueEnum};
use ecp_core::EcpError;

#[derive(Args, Debug, Clone)]
pub struct HookArgs {
    /// Which Claude Code hook event fired.
    #[arg(value_enum)]
    pub event: HookEvent,

    /// Identifies the agent host whose envelope shape stdin carries.
    /// Exactly one host flag must be set; absence is an error so we
    /// don't silently misinterpret stdin from a different host.
    #[arg(long, default_value_t = false)]
    pub claude_code: bool,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
#[clap(rename_all = "kebab-case")]
pub enum HookEvent {
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    SessionStart,
    /// PreToolUse(Agent|Task) tripwire: redirect structural code queries
    /// from agent dispatch to ecp verbs.
    AgentDispatch,
}

pub fn run(args: HookArgs) -> Result<(), EcpError> {
    if !args.claude_code {
        return Err(EcpError::InvalidArgument(
            "ecp hook: exactly one host flag required (e.g. --claude-code)".into(),
        ));
    }
    let input = common::read_stdin_envelope()?;
    // Any hook event means an agent is doing something in this worktree, which
    // is the signal peers actually needs: an agent editing for an hour without
    // running a graph-backed command is exactly when its dirty surface is
    // growing, and it must not expire out of `peers status` while that happens.
    // Costs one stat per event; the write is throttled to once a minute and a
    // session that was never enrolled is left alone.
    crate::auto_ensure::beat_session_heartbeat(std::path::Path::new(&input.cwd));
    match args.event {
        HookEvent::UserPromptSubmit => user_prompt_submit::handle(&input),
        HookEvent::PreToolUse => pre_tool_use::handle(&input),
        HookEvent::PostToolUse => post_tool_use::handle(&input),
        HookEvent::SessionStart => session_start::handle(&input),
        HookEvent::AgentDispatch => agent_dispatch::handle(&input),
    }
}
