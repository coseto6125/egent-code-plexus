//! `cgn hook <event> --claude-code` — Claude Code hook entry point.
//!
//! Reads a JSON envelope on stdin, dispatches to the per-event handler,
//! and emits a `{"hookSpecificOutput": ...}` JSON response on stdout
//! (empty stdout means no-op — Claude Code treats that as "nothing to
//! add to the conversation").
//!
//! Per-event logic lives in sibling modules so each handler is a
//! self-contained file. Shared utilities (stdin parse, response emit,
//! marker paths, shell-quote stripping) live in `common`.

pub mod common;
pub mod post_tool_use;
pub mod pre_tool_use;
pub mod session_start;
pub mod user_prompt_submit;

use clap::{Args, ValueEnum};
use cgn_core::CgnError;

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
}

pub fn run(args: HookArgs) -> Result<(), CgnError> {
    if !args.claude_code {
        return Err(CgnError::InvalidArgument(
            "cgn hook: exactly one host flag required (e.g. --claude-code)".into(),
        ));
    }
    let input = common::read_stdin_envelope()?;
    match args.event {
        HookEvent::UserPromptSubmit => user_prompt_submit::handle(&input),
        HookEvent::PreToolUse => pre_tool_use::handle(&input),
        HookEvent::PostToolUse => post_tool_use::handle(&input),
        HookEvent::SessionStart => session_start::handle(&input),
    }
}
