//! Shared subprocess wrapper for invoking the `gemini` CLI binary.

use cgn_core::CgnError;
use std::process::{Command, Output};

pub(crate) fn run(args: &[&str]) -> Result<Output, CgnError> {
    Command::new("gemini")
        .args(args)
        .output()
        .map_err(|e| CgnError::Output(format!("failed to spawn gemini: {e}")))
}
