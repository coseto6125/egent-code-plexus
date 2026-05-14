//! Registry: central name registry, per-branch metadata, audit log.
//! See spec §1-§2, §9.

mod audit;
mod io;
mod lock;
mod meta;
mod path;
mod store;

pub use audit::{AuditEvent, AuditLog};
/// Internal implementation detail. Not part of public API.
/// Use only within gnx-core or in tests.
#[doc(hidden)]
pub use lock::FileLock;
pub use meta::BranchMeta;
pub use path::{
    derive_repo_name, sanitize_branch, sanitize_segment, uid_path,
    IndexLayout, PathError,
};
pub use store::{
    strip_credentials, BranchEntry, GroupEntry, RegistryFile, RepoEntry,
};

use std::path::{Path, PathBuf};

/// High-level registry handle. Holds the directory root; reads/writes
/// registry.json under flock protection.
pub struct Registry {
    home_gnx: PathBuf,
    in_memory: RegistryFile,
}

impl Registry {
    /// Open / lazily create `~/.gnx/registry.json`. Reads with .bak
    /// fallback (spec §2.1).
    pub fn open(home_gnx: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(home_gnx)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(home_gnx)?.permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(home_gnx, perms)?;
        }
        let path = home_gnx.join("registry.json");
        let in_memory = RegistryFile::read_or_empty(&path)?;
        Ok(Self {
            home_gnx: home_gnx.to_path_buf(),
            in_memory,
        })
    }

    /// Read-only view of current state.
    pub fn snapshot(&self) -> &RegistryFile {
        &self.in_memory
    }

    /// Insert or update a repo entry. Holds exclusive flock for
    /// the entire read-modify-write cycle.
    pub fn upsert_repo(&mut self, entry: RepoEntry) -> std::io::Result<()> {
        let lock_path = self.home_gnx.join("registry.json.lock");
        let _lock = FileLock::acquire_exclusive(&lock_path)?;

        // Re-read in case another process changed it
        let registry_path = self.home_gnx.join("registry.json");
        let mut current = RegistryFile::read_or_empty(&registry_path)?;

        if let Some(existing) = current.repos.iter_mut().find(|r| r.name == entry.name) {
            *existing = entry;
        } else {
            current.repos.push(entry);
        }

        RegistryFile::write_atomic(&registry_path, &current)?;
        self.in_memory = current;
        Ok(())
    }
}
