//! Registry: central name registry, per-branch metadata, audit log.
//! See spec §1-§2, §9.

mod path;

pub use path::{
    derive_repo_name, sanitize_branch, sanitize_segment, uid_path,
    IndexLayout, PathError,
};
