//! Stub — implemented in Task 12 (say / inbox / thread).

use std::path::Path;

pub fn cmd_say(_: &Path, _: &str, _: Option<&str>, _: Option<&str>) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "`peers say` not yet implemented (Task 12)",
    ))
}
pub fn cmd_inbox(_: &Path, _: usize) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "`peers inbox` not yet implemented (Task 12)",
    ))
}
pub fn cmd_thread(_: &Path, _: &str) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "`peers thread` not yet implemented (Task 12)",
    ))
}
