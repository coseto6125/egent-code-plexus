//! Cross-session test fixture: spawn N cgn watcher processes against a shared temp repo.

#![allow(dead_code)]

use cgn_core::peer::inbox::{drain, InboxEntry};
use cgn_core::session::SessionMeta;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

pub struct PeerHarness {
    pub repo_root: TempDir,
    pub watchers: Vec<SpawnedSession>,
}

pub struct SpawnedSession {
    pub id: String,
    pub pid: u32,
    pub session_dir: PathBuf,
    pub child: Option<Child>,
}

impl PeerHarness {
    pub fn new() -> Self {
        let repo_root = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(repo_root.path().join("sessions")).unwrap();
        Self {
            repo_root,
            watchers: Vec::new(),
        }
    }

    pub fn spawn_session(&mut self, id: &str) -> &SpawnedSession {
        let session_dir = self.repo_root.path().join("sessions").join(id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let meta = SessionMeta {
            version: 1,
            session_id: id.into(),
            pid: Some(std::process::id()),
            started_at: Utc::now().to_rfc3339(),
            last_touched: Utc::now().to_rfc3339(),
            base_sha: "0".repeat(40),
            source_worktree: "/tmp".into(),
            overlay_version: 1,
            watcher_pid: None,
            last_drained_offset: 0,
        };
        SessionMeta::write_atomic(&session_dir.join("meta.json"), &meta).unwrap();

        let bin: PathBuf = env!("CARGO_BIN_EXE_cgn").into();
        let child = Command::new(&bin)
            .args([
                "watch",
                "--foreground",
                "--repo",
                self.repo_root.path().to_str().unwrap(),
            ])
            .env("CGN_SESSION_ID", id)
            .env("CLAUDE_CODE_SESSION_ID", id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn watcher");
        let pid = child.id();
        self.watchers.push(SpawnedSession {
            id: id.into(),
            pid,
            session_dir,
            child: Some(child),
        });
        self.watchers.last().unwrap()
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.repo_root.path().join("sessions").join(id)
    }

    pub fn write_dirty(&self, id: &str, path: &str, symbols: &[(&str, &str)]) {
        use cgn_core::session::overlay::{DirtyEntry, DirtyFiles, SymbolKind, SymbolRef};
        use std::collections::BTreeMap;
        let sdir = self.session_dir(id);
        let mut entries = BTreeMap::new();
        entries.insert(
            path.to_string(),
            DirtyEntry {
                mtime_ns: 1,
                content_hash: "h".into(),
                fragment_id: "f".into(),
                tantivy_delta_segment: None,
                parse_failed: false,
                dirty_symbols: symbols
                    .iter()
                    .map(|(n, f)| SymbolRef {
                        name: (*n).into(),
                        kind: SymbolKind::Function,
                        file: (*f).into(),
                        line_start: 1,
                        line_end: 10,
                    })
                    .collect(),
            },
        );
        DirtyFiles::write_atomic(
            &sdir.join("dirty_files.json"),
            &DirtyFiles {
                version: 1,
                entries,
            },
        )
        .unwrap();
    }

    pub fn read_inbox(&self, id: &str) -> Vec<InboxEntry> {
        let (entries, _) = drain(&self.session_dir(id).join("inbox.jsonl"), 0).unwrap();
        entries
    }

    pub fn assert_within<F: Fn() -> bool>(&self, timeout: Duration, pred: F) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if pred() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    pub fn say(&self, from: &str, to: Option<&str>, body: &str) -> std::process::Output {
        let bin: PathBuf = env!("CARGO_BIN_EXE_cgn").into();
        let mut args = vec![
            "peers",
            "say",
            body,
            "--repo",
            self.repo_root.path().to_str().unwrap(),
        ];
        if let Some(t) = to {
            args.push("--to");
            args.push(t);
        }
        Command::new(bin)
            .args(&args)
            .env("CGN_SESSION_ID", from)
            .env("CLAUDE_CODE_SESSION_ID", from)
            .output()
            .expect("spawn cgn peers say")
    }
}

impl Drop for PeerHarness {
    fn drop(&mut self) {
        for w in &mut self.watchers {
            if let Some(child) = w.child.as_mut() {
                #[cfg(unix)]
                {
                    use nix::sys::signal::{kill, Signal};
                    use nix::unistd::Pid;
                    let _ = kill(Pid::from_raw(w.pid as i32), Signal::SIGTERM);
                }
                #[cfg(windows)]
                {
                    let _ = child.kill();
                }
                let _ = child.wait();
            }
        }
    }
}

#[allow(dead_code)]
const _ASSERT_PATH: fn(&Path) = |_| {};
