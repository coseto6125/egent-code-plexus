//! Audit log: JSON Lines, append-only, mutation events only.
//! Spec §9.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum AuditEvent {
    #[serde(rename = "analyze.start")]
    AnalyzeStart { repo: String, branch: String },
    #[serde(rename = "analyze.complete")]
    AnalyzeComplete {
        repo: String,
        branch: String,
        files: u32,
        nodes: u32,
        duration_ms: u64,
    },
    #[serde(rename = "rename.execute")]
    RenameExecute {
        target: String,
        kind: String,
        affected_files: u32,
        dry_run: bool,
    },
    #[serde(rename = "hook.fired")]
    HookFired {
        #[serde(rename = "type")]
        kind: String,
        from: Option<String>,
        to: Option<String>,
        repo: String,
    },
    #[serde(rename = "registry.mutate")]
    RegistryMutate {
        op: String,
        repo: String,
        branch: Option<String>,
    },
    #[serde(rename = "oom.aborted")]
    OomAborted { phase: String, peak_rss_mb: u64 },
}

impl AuditEvent {
    /// Serialize as one JSON Lines record (ending with `\n`).
    /// Inserts current ts (RFC3339 UTC) as the first field via flatten envelope.
    pub fn to_json_line(&self) -> io::Result<String> {
        #[derive(Serialize)]
        struct Envelope<'a> {
            ts: String,
            #[serde(flatten)]
            event: &'a AuditEvent,
        }
        let env = Envelope {
            ts: chrono::Utc::now().to_rfc3339(),
            event: self,
        };
        let line = serde_json::to_string(&env).map_err(io::Error::other)?;
        Ok(format!("{line}\n"))
    }
}

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const KEEP_ROTATED: u32 = 2;

/// Append-only audit log handle. Single thread per process.
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    /// Open or create the audit log at `path`. Caller may then `append`
    /// repeatedly; each `append` re-opens the file briefly (POSIX-safe
    /// short append).
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Append one event after rotating if the file exceeds MAX_BYTES.
    pub fn append(&self, event: &AuditEvent) -> io::Result<()> {
        self.rotate_if_needed()?;
        let line = event.to_json_line()?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())?;
        Ok(())
    }

    fn rotate_if_needed(&self) -> io::Result<()> {
        let size = match fs::metadata(&self.path) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if size < MAX_BYTES {
            return Ok(());
        }

        // Shift: .2 deleted (if would become .3), .1 → .2, current → .1
        for n in (1..=KEEP_ROTATED).rev() {
            let from = rotated_path(&self.path, n);
            let to = rotated_path(&self.path, n + 1);
            if from.exists() {
                if n == KEEP_ROTATED {
                    let _ = fs::remove_file(&from);
                } else {
                    crate::registry::rename_with_retry(&from, &to)?;
                }
            }
        }
        crate::registry::rename_with_retry(&self.path, &rotated_path(&self.path, 1))?;
        Ok(())
    }
}

impl AuditLog {
    /// Delete rotated files older than `max_age`. Caller should invoke
    /// periodically (e.g. on each `append`). Cheap when no rotation
    /// has happened recently.
    pub fn cleanup_old(&self, max_age: std::time::Duration) -> io::Result<()> {
        let cutoff = std::time::SystemTime::now() - max_age;
        for n in 1..=KEEP_ROTATED {
            let p = rotated_path(&self.path, n);
            if !p.exists() {
                continue;
            }
            let m = fs::metadata(&p)?;
            if let Ok(mt) = m.modified() {
                if mt < cutoff {
                    let _ = fs::remove_file(&p);
                }
            }
        }
        Ok(())
    }
}

fn rotated_path(base: &Path, n: u32) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}
