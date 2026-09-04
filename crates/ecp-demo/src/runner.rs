//! Spawn one `ecp` invocation per request with the guards a public endpoint
//! needs: a wall-clock timeout that kills the child, an output cap, and a
//! concurrency cap sized for the instance's CPU share.

use crate::tools::DemoTool;
use ecp_mcp::spawn::build_argv;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub struct Runner {
    bin: PathBuf,
    timeout: Duration,
    max_output_bytes: usize,
    permits: Arc<Semaphore>,
}

#[derive(Debug, Serialize)]
pub struct Outcome {
    /// The command as an agent would type it: `ecp` plus every token.
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug)]
pub enum RunError {
    /// The JSON args do not translate to argv (unknown shape, bad `subcmd`).
    BadArgs(String),
    Spawn(std::io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::BadArgs(msg) => write!(f, "invalid args: {msg}"),
            RunError::Spawn(e) => write!(f, "spawning ecp: {e}"),
        }
    }
}

impl Runner {
    pub fn new(
        bin: PathBuf,
        timeout: Duration,
        max_output_bytes: usize,
        concurrency: usize,
    ) -> Self {
        Self {
            bin,
            timeout,
            max_output_bytes,
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    /// Run `ecp <sub> [--repo <corpus>] <argv>` inside `corpus`.
    pub async fn run(
        &self,
        tool: &DemoTool,
        corpus: &Path,
        args: &Value,
    ) -> Result<Outcome, RunError> {
        let mut argv = vec![tool.inner.subcommand.clone()];
        if tool.takes_repo {
            argv.push("--repo".into());
            argv.push(corpus.display().to_string());
        }
        argv.extend(build_argv(&tool.inner, args).map_err(|e| RunError::BadArgs(e.to_string()))?);

        // A closed semaphore is impossible here: the runner owns it for its whole life.
        let _permit = self
            .permits
            .acquire()
            .await
            .expect("runner semaphore stays open");
        let started = Instant::now();
        let child = tokio::process::Command::new(&self.bin)
            .args(&argv)
            .current_dir(corpus)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(RunError::Spawn)?;

        let mut display_argv = Vec::with_capacity(argv.len() + 1);
        display_argv.push("ecp".to_string());
        display_argv.extend(argv);

        // Dropping the timed-out future drops the child, and `kill_on_drop`
        // turns that into SIGKILL. Nothing else stops a runaway cypher query.
        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                let (stdout, truncated) = self.capped(&output.stdout);
                let (stderr, _) = self.capped(&output.stderr);
                Ok(Outcome {
                    argv: display_argv,
                    exit_code: output.status.code(),
                    stdout,
                    stderr,
                    truncated,
                    timed_out: false,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                })
            }
            Ok(Err(e)) => Err(RunError::Spawn(e)),
            Err(_) => Ok(Outcome {
                argv: display_argv,
                exit_code: None,
                stdout: String::new(),
                stderr: format!("killed after {}s", self.timeout.as_secs()),
                truncated: false,
                timed_out: true,
                elapsed_ms: started.elapsed().as_millis() as u64,
            }),
        }
    }

    fn capped(&self, bytes: &[u8]) -> (String, bool) {
        let cut = bytes.len() > self.max_output_bytes;
        let kept = if cut {
            &bytes[..self.max_output_bytes]
        } else {
            bytes
        };
        (String::from_utf8_lossy(kept).into_owned(), cut)
    }
}
