//! Registry: central name registry, per-branch metadata, audit log.
//! See spec §1-§2, §9.

mod audit;
pub mod commit_meta;
pub mod dirname;
pub(crate) mod io;
mod lock;
mod path;
pub mod repo_meta;
mod store;

pub use audit::{AuditEvent, AuditLog};
pub use commit_meta::{CommitBuildMeta, EmbeddingStatus, RefRecord, BUILDER_FINGERPRINT};
pub use dirname::{CommitDirName, ParseError as DirNameParseError, SourceType};
pub use io::{atomic_write_bytes, atomic_write_json};
/// Internal implementation detail. Not part of public API.
/// Use only within graph-nexus-core or in tests.
#[doc(hidden)]
pub use lock::FileLock;
pub use path::{derive_repo_name, resolve_home_gnx, sanitize_segment, uid_path, PathError};
pub use repo_meta::RepoMeta;
pub use store::{strip_credentials, GroupEntry, RegistryFile, RepoAlias, CURRENT_VERSION};

use std::path::{Path, PathBuf};

/// High-level registry handle. Holds the directory root; reads/writes
/// registry.json under flock protection.
pub struct Registry {
    home_gnx: PathBuf,
    in_memory: RegistryFile,
}

impl Registry {
    /// Open / lazily create `~/.gnx/registry.json`. On parse failure,
    /// callers should invoke `RegistryFile::rebuild_from_disk` for recovery
    /// (walks per-repo meta.json files; spec §12 Error Handling).
    /// `.bak` is written by `write_atomic` as a snapshot but never read back.
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

    /// Insert or update a repo alias entry. Holds exclusive flock for
    /// the entire read-modify-write cycle.
    pub fn upsert_repo(&mut self, entry: RepoAlias) -> std::io::Result<()> {
        let lock_path = self.home_gnx.join("registry.json.lock");
        let _lock = FileLock::acquire_exclusive(&lock_path)?;

        // Re-read in case another process changed it
        let registry_path = self.home_gnx.join("registry.json");
        let mut current = RegistryFile::read_or_empty(&registry_path)?;

        current.repos.insert(entry.dir_name.clone(), entry);

        RegistryFile::write_atomic(&registry_path, &current)?;
        self.in_memory = current;
        Ok(())
    }
}
