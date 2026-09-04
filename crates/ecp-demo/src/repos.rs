//! On-demand corpora. A public GitHub repository is cloned at the tip of its
//! default branch, indexed once by `ecp admin index`, then served until the
//! store evicts it. Every step that touches the network or the CPU runs under
//! a timeout, one build at a time, and a size ceiling is enforced twice: from
//! the GitHub API before the clone, and from the checkout after it.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Cloning,
    Indexing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoEntry {
    /// `owner/repo`, the id the browser sends back.
    pub name: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub status: Status,
    pub error: Option<String>,
    /// `ecp summary --format json` once `Ready`; `null` before that.
    pub summary: Value,
    pub bytes: u64,
    pub commit: Option<String>,
    pub added_at: u64,
    #[serde(skip)]
    pub last_used: Instant,
}

/// External programs the store spawns. Injected so tests can stand in for all three.
#[derive(Debug, Clone)]
pub struct Programs {
    pub ecp: PathBuf,
    pub git: PathBuf,
    pub curl: PathBuf,
}

#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub dir: PathBuf,
    pub programs: Programs,
    pub max_repo_kb: u64,
    pub max_repos: usize,
    pub queue_limit: usize,
    pub clone_timeout: Duration,
    pub index_timeout: Duration,
    pub github_token: Option<String>,
}

pub struct RepoStore {
    cfg: StoreConfig,
    entries: Mutex<Vec<RepoEntry>>,
    build_slot: Semaphore,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddError {
    BadUrl(String),
    QueueFull,
}

impl std::fmt::Display for AddError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddError::BadUrl(msg) => write!(f, "{msg}"),
            AddError::QueueFull => write!(
                f,
                "too many repositories are being indexed right now; retry in a minute"
            ),
        }
    }
}

/// `owner/repo` out of anything a person pastes: a full URL, `github.com/o/r`,
/// `o/r`, with or without `.git`, a trailing slash or a `/tree/...` suffix.
pub fn parse_github_repo(input: &str) -> Result<(String, String), String> {
    let s = input.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.strip_prefix("www.").unwrap_or(s);
    let s = s.strip_prefix("github.com/").unwrap_or(s);
    let mut parts = s.split('/').filter(|p| !p.is_empty());
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    // GitHub accounts are alphanumeric plus hyphens, so a host name such as
    // `gitlab.com` never passes as an owner. Repo names may also carry `.`/`_`.
    let valid_owner =
        !owner.is_empty() && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let valid_repo = !repo.is_empty()
        && repo != "."
        && repo != ".."
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid_owner || !valid_repo {
        return Err(format!(
            "{input:?} is not a GitHub repository; use https://github.com/<owner>/<repo>"
        ));
    }
    Ok((owner.to_string(), repo.to_string()))
}

impl RepoStore {
    pub fn new(cfg: StoreConfig) -> Self {
        Self {
            cfg,
            entries: Mutex::new(Vec::new()),
            build_slot: Semaphore::new(1),
        }
    }

    pub fn config(&self) -> &StoreConfig {
        &self.cfg
    }

    pub fn list(&self) -> Vec<RepoEntry> {
        self.lock().clone()
    }

    /// The checkout path of a `Ready` repo; marks it as just used.
    pub fn ready_path(&self, name: &str) -> Result<PathBuf, Option<Status>> {
        let mut entries = self.lock();
        let entry = entries.iter_mut().find(|e| e.name == name).ok_or(None)?;
        if entry.status != Status::Ready {
            return Err(Some(entry.status));
        }
        entry.last_used = Instant::now();
        Ok(entry.path.clone())
    }

    /// Register `owner/repo` and start its build in the background. An entry
    /// that already exists is returned as is (re-adding is a no-op, a failed
    /// one is retried).
    pub fn add(self: &Arc<Self>, owner: &str, repo: &str) -> Result<RepoEntry, AddError> {
        let name = format!("{owner}/{repo}");
        let path = self.cfg.dir.join(format!("{owner}__{repo}"));
        let entry = {
            let mut entries = self.lock();
            if let Some(existing) = entries.iter_mut().find(|e| e.name == name) {
                if existing.status != Status::Failed {
                    return Ok(existing.clone());
                }
                existing.status = Status::Queued;
                existing.error = None;
                existing.clone()
            } else {
                let pending = entries.iter().filter(|e| e.status != Status::Ready).count();
                if pending >= self.cfg.queue_limit {
                    return Err(AddError::QueueFull);
                }
                let entry = RepoEntry {
                    name: name.clone(),
                    path,
                    status: Status::Queued,
                    error: None,
                    summary: Value::Null,
                    bytes: 0,
                    commit: None,
                    added_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    last_used: Instant::now(),
                };
                entries.push(entry.clone());
                entry
            }
        };
        let store = Arc::clone(self);
        tokio::spawn(async move { store.build(name).await });
        Ok(entry)
    }

    async fn build(self: Arc<Self>, name: String) {
        // The semaphore lives as long as the store; a closed one is impossible.
        let _slot = self
            .build_slot
            .acquire()
            .await
            .expect("build slot stays open");
        let path = match self.lock().iter().find(|e| e.name == name) {
            Some(e) => e.path.clone(),
            None => return,
        };
        self.evict_for(&name).await;
        match self.clone_and_index(&name, &path).await {
            Ok((bytes, commit, summary)) => self.update(&name, |e| {
                e.status = Status::Ready;
                e.error = None;
                e.bytes = bytes;
                e.commit = commit;
                e.summary = summary;
                e.last_used = Instant::now();
            }),
            Err(msg) => {
                self.discard(&path).await;
                self.update(&name, |e| {
                    e.status = Status::Failed;
                    e.error = Some(msg);
                });
            }
        }
    }

    async fn clone_and_index(
        &self,
        name: &str,
        path: &Path,
    ) -> Result<(u64, Option<String>, Value), String> {
        self.update(name, |e| e.status = Status::Cloning);
        self.discard(path).await;
        self.precheck(name).await?;

        let url = format!("https://github.com/{name}.git");
        let mut clone = Command::new(&self.cfg.programs.git);
        clone
            .args(["clone", "--quiet", "--depth", "1", "--single-branch", &url])
            .arg(path)
            .env("GIT_TERMINAL_PROMPT", "0");
        run(clone, self.cfg.clone_timeout)
            .await
            .map_err(|e| format!("clone: {e}"))?;

        let bytes = dir_bytes(path);
        let cap = self.cfg.max_repo_kb * 1024;
        if bytes > cap {
            return Err(format!(
                "checkout is {} MB, above the {} MB ceiling of this demo instance",
                bytes / (1024 * 1024),
                cap / (1024 * 1024)
            ));
        }
        let mut head = Command::new(&self.cfg.programs.git);
        head.args(["rev-parse", "--short", "HEAD"])
            .current_dir(path);
        let commit = run(head, Duration::from_secs(10))
            .await
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty());

        self.update(name, |e| e.status = Status::Indexing);
        let mut index = Command::new(&self.cfg.programs.ecp);
        index
            .args(["admin", "index", "--repo"])
            .arg(path)
            .current_dir(path);
        run(index, self.cfg.index_timeout)
            .await
            .map_err(|e| format!("index: {e}"))?;

        let mut summary = Command::new(&self.cfg.programs.ecp);
        summary
            .args(["summary", "--format", "json", "--repo"])
            .arg(path)
            .current_dir(path);
        let summary = run(summary, Duration::from_secs(30))
            .await
            .ok()
            .and_then(|out| serde_json::from_slice(&out.stdout).ok())
            .unwrap_or(Value::Null);
        Ok((bytes, commit, summary))
    }

    /// Ask the GitHub API for size and visibility before spending bandwidth.
    /// A rate-limited or unreachable API is not a refusal: the post-clone
    /// size check still applies.
    async fn precheck(&self, name: &str) -> Result<(), String> {
        let mut curl = Command::new(&self.cfg.programs.curl);
        curl.args([
            "-sS",
            "-w",
            "\n%{http_code}",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: ecp-demo",
        ]);
        if let Some(token) = &self.cfg.github_token {
            curl.arg("-H").arg(format!("Authorization: Bearer {token}"));
        }
        curl.arg(format!("https://api.github.com/repos/{name}"));
        let Ok(out) = run(curl, Duration::from_secs(15)).await else {
            return Ok(());
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, code) = text.rsplit_once('\n').unwrap_or((&text, ""));
        match code.trim() {
            "404" => {
                return Err(format!(
                    "github.com/{name} was not found (private repositories are not supported)"
                ))
            }
            "200" => {}
            _ => return Ok(()),
        }
        let info: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let size_kb = info.get("size").and_then(Value::as_u64).unwrap_or(0);
        if size_kb > self.cfg.max_repo_kb {
            return Err(format!(
                "GitHub reports {} MB; this demo instance indexes repositories up to {} MB",
                size_kb / 1024,
                self.cfg.max_repo_kb / 1024
            ));
        }
        Ok(())
    }

    /// Drop the least recently used `Ready` repos until one more fits.
    async fn evict_for(&self, incoming: &str) {
        loop {
            let victim = {
                let entries = self.lock();
                let ready = entries.iter().filter(|e| e.status == Status::Ready).count();
                if ready < self.cfg.max_repos {
                    break;
                }
                entries
                    .iter()
                    .filter(|e| e.status == Status::Ready && e.name != incoming)
                    .min_by_key(|e| e.last_used)
                    .map(|e| (e.name.clone(), e.path.clone()))
            };
            let Some((name, path)) = victim else { break };
            self.discard(&path).await;
            self.lock().retain(|e| e.name != name);
        }
    }

    /// Remove a checkout and its index. Both steps tolerate absence.
    async fn discard(&self, path: &Path) {
        if !path.exists() {
            return;
        }
        let mut drop = Command::new(&self.cfg.programs.ecp);
        drop.args(["admin", "drop", "--repo"])
            .arg(path)
            .current_dir(&self.cfg.dir);
        let _ = run(drop, Duration::from_secs(30)).await;
        let _ = tokio::fs::remove_dir_all(path).await;
    }

    fn update(&self, name: &str, f: impl FnOnce(&mut RepoEntry)) {
        if let Some(entry) = self.lock().iter_mut().find(|e| e.name == name) {
            f(entry);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<RepoEntry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Run to completion under `timeout`; a non-zero exit carries stderr.
async fn run(mut cmd: Command, timeout: Duration) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) if out.status.success() => Ok(out),
        Ok(Ok(out)) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("gave up after {}s", timeout.as_secs())),
    }
}

fn dir_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_bytes(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_repo_accepts_every_shape_people_paste() {
        for input in [
            "https://github.com/rust-lang/cargo",
            "http://github.com/rust-lang/cargo/",
            "https://www.github.com/rust-lang/cargo.git",
            "github.com/rust-lang/cargo/tree/master/src",
            "rust-lang/cargo",
            "  rust-lang/cargo.git  ",
        ] {
            assert_eq!(
                parse_github_repo(input),
                Ok(("rust-lang".to_string(), "cargo".to_string())),
                "{input}"
            );
        }
    }

    #[test]
    fn parse_github_repo_rejects_other_hosts_and_path_tricks() {
        for input in [
            "https://gitlab.com/a/b",
            "https://github.com/a",
            "../etc/passwd",
            "a/../b",
            "a/b c",
            "",
            "https://github.com/",
        ] {
            assert!(
                parse_github_repo(input).is_err(),
                "{input:?} must be rejected"
            );
        }
    }
}
