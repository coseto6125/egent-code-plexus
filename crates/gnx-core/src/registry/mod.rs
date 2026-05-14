//! Registry: central name registry, per-branch metadata, audit log.
//! See spec §1-§2, §9.

mod audit;
mod lock;
mod meta;
mod path;
mod store;

pub use audit::{AuditEvent, AuditLog};
pub use lock::FileLock;
pub use meta::BranchMeta;
pub use path::{
    derive_repo_name, sanitize_branch, sanitize_segment, uid_path,
    IndexLayout, PathError,
};
pub use store::{
    strip_credentials, BranchEntry, GroupEntry, RegistryFile, RepoEntry,
};
