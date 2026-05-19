pub mod diff_parser;
pub mod provider;
pub mod safe_exec;
pub mod shell;

pub use diff_parser::{parse_diff_hunks, FileDiff};
pub use provider::{DiffScope, GitDiffProvider};
pub use shell::ShellGitProvider;
