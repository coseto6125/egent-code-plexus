//! Shared subprocess wrapper for invoking the `gemini` CLI binary.

use ecp_core::EcpError;
use std::process::{Command, Output};

pub(crate) fn run(args: &[&str]) -> Result<Output, EcpError> {
    Command::new("gemini")
        .args(args)
        .output()
        .map_err(|e| EcpError::Output(format!("failed to spawn gemini: {e}")))
}
