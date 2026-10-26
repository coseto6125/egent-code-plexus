//! Cross-session test fixture: spawn N ecp watcher processes against a shared temp repo.

#![allow(dead_code)]

use chrono::Utc;
use ecp_core::peer::inbox::{drain, InboxEntry};
use ecp_core::session::SessionMeta;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

pub struct PeerHarness {
    pub repo_root: TempDir,
    pub watchers: Vec<SpawnedSession>,
    /// Set by [`PeerHarness::backed_by`] when the test needs the watcher to
    /// reach a real indexed graph (SOFT concerns); `None` keeps the default
    /// no-graph setup every other peers test runs on.
    graph: Option<GraphBacking>,
}

struct GraphBacking {
    home: PathBuf,
    worktree: PathBuf,
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
            graph: None,
        }
    }

    /// Point every session spawned afterwards at an indexed worktree: sessions
    /// record it as `source_worktree` and watchers run with `HOME=home`, which
    /// is how the watcher resolves the published graph for SOFT concerns.
    pub fn backed_by(mut self, home: &Path, worktree: &Path) -> Self {
        self.graph = Some(GraphBacking {
            home: home.to_path_buf(),
            worktree: worktree.to_path_buf(),
        });
        self
    }

    pub fn spawn_session(&mut self, id: &str) -> &SpawnedSession {
        self.spawn_session_named(id, None)
    }

    pub fn spawn_session_named(&mut self, id: &str, agent_name: Option<&str>) -> &SpawnedSession {
        let session_dir = self.repo_root.path().join("sessions").join(id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let meta = SessionMeta {
            version: 1,
            session_id: id.into(),
            pid: Some(std::process::id()),
            started_at: Utc::now().to_rfc3339(),
            last_touched: Utc::now().to_rfc3339(),
            base_sha: "0".repeat(40),
            source_worktree: match &self.graph {
                Some(g) => g.worktree.to_string_lossy().into_owned(),
                None => "/tmp".into(),
            },
            overlay_version: 1,
            watcher_pid: None,
            last_drained_offset: 0,
            agent_name: agent_name.map(str::to_string),
        };
        SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &meta).unwrap();

        let bin: PathBuf = env!("CARGO_BIN_EXE_ecp").into();
        let mut cmd = Command::new(&bin);
        cmd.args([
            "watch",
            "--foreground",
            "--repo",
            self.repo_root.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", id)
        .env("CLAUDE_CODE_SESSION_ID", id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(self.stderr_path(id)).unwrap());
        if let Some(g) = &self.graph {
            cmd.env("HOME", &g.home)
                .env_remove("ECP_HOME")
                .env("ECP_SKIP_BG_REBUILD", "1");
        }
        let child = cmd.spawn().expect("spawn watcher");
        let pid = child.id();
        self.watchers.push(SpawnedSession {
            id: id.into(),
            pid,
            session_dir,
            child: Some(child),
        });
        self.watchers.last().unwrap()
    }

    /// Outside `sessions/`: a log the watchers watch would feed them their
    /// own writes as events.
    fn stderr_path(&self, id: &str) -> PathBuf {
        let dir = self.repo_root.path().join("watcher-logs");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{id}.stderr"))
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.repo_root.path().join("sessions").join(id)
    }

    pub fn write_dirty(&self, id: &str, path: &str, symbols: &[(&str, &str)]) {
        use ecp_core::contracts::dirty_event::{PathChange, PathClass};
        use ecp_core::contracts::routing::SizeCap;
        use ecp_core::session::layout;
        use ecp_core::session::overlay::{DirtyEventEntry, DirtyEvents, SymbolKind, SymbolRef};
        use std::collections::BTreeMap;
        let sdir = self.session_dir(id);
        let mut entries = BTreeMap::new();
        entries.insert(
            path.to_string(),
            DirtyEventEntry {
                change: PathChange::Modified,
                class: PathClass::SOURCE,
                mtime_ns: 1,
                content_hash: Some("h".into()),
                fragment_id: Some("f".into()),
                parse_failed: false,
                format: 1,
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
        let mut manifest = DirtyEvents::empty(SizeCap::BUILT_IN);
        manifest.entries = entries;
        DirtyEvents::write_atomic(&layout::dirty_events_path(&sdir), &manifest).unwrap();
    }

    pub fn read_inbox(&self, id: &str) -> Vec<InboxEntry> {
        let (entries, _) = drain(&self.session_dir(id).join("inbox.jsonl"), 0).unwrap();
        entries
    }

    /// Per-session state for a failed fan-out assertion: whether the watcher
    /// process is still alive, which peers its inbox heard from, and the tail
    /// of its stderr. The fan-out flake reproduces only on CI runners.
    pub fn diagnostics(&mut self) -> String {
        let mut out = String::new();
        for i in 0..self.watchers.len() {
            let id = self.watchers[i].id.clone();
            let status = match self.watchers[i].child.as_mut().map(Child::try_wait) {
                Some(Ok(Some(st))) => format!("exited {st}"),
                Some(Ok(None)) => "running".into(),
                Some(Err(e)) => format!("try_wait error {e}"),
                None => "no child".into(),
            };
            let mut froms: Vec<String> = self
                .read_inbox(&id)
                .into_iter()
                .filter_map(|e| match e {
                    InboxEntry::DirtyEvent { peer_session, .. } => Some(peer_session),
                    _ => None,
                })
                .collect();
            froms.sort();
            froms.dedup();
            let stderr = std::fs::read_to_string(self.stderr_path(&id)).unwrap_or_default();
            let lines: Vec<&str> = stderr.lines().collect();
            let tail = if lines.len() <= 50 {
                lines.join("\n    ")
            } else {
                format!(
                    "{}\n    ...\n    {}",
                    lines[..25].join("\n    "),
                    lines[lines.len() - 25..].join("\n    ")
                )
            };
            out.push_str(&format!(
                "\n[{id}] watcher {status}; heard from {froms:?}\n    {tail}"
            ));
        }
        out
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
        let bin: PathBuf = env!("CARGO_BIN_EXE_ecp").into();
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
            .env("ECP_SESSION_ID", from)
            .env("CLAUDE_CODE_SESSION_ID", from)
            .output()
            .expect("spawn ecp peers say")
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
