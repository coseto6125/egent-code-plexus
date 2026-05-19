//! GitDiffProvider trait — abstracts the diff source so detect_changes can
//! be unit-tested without a real git repository.

use super::FileDiff;
use cgn_core::GnxError;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffScope {
    Unstaged,
    Staged,
    All,
    Compare(String),
}

impl DiffScope {
    pub fn parse(scope: Option<&str>, base_ref: Option<&str>) -> Result<Self, GnxError> {
        match scope.unwrap_or("unstaged") {
            "unstaged" => Ok(DiffScope::Unstaged),
            "staged" => Ok(DiffScope::Staged),
            "all" => Ok(DiffScope::All),
            "compare" => {
                let r = base_ref.ok_or_else(|| {
                    GnxError::InvalidArgument("base_ref is required for scope=compare".to_string())
                })?;
                Ok(DiffScope::Compare(r.to_string()))
            }
            other => Err(GnxError::InvalidArgument(format!(
                "unknown scope '{other}' (expected unstaged|staged|all|compare)"
            ))),
        }
    }
}

pub trait GitDiffProvider: Send + Sync {
    fn diff(&self, repo: &Path, scope: &DiffScope) -> Result<Vec<FileDiff>, GnxError>;
}
