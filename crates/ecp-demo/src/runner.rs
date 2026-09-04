//! Spawn one `ecp` invocation per request with the guards a public endpoint
//! needs: the server-owned flags rejected on the final argv, a bounded wait
//! for a concurrency permit, a wall-clock timeout that kills the child, and
//! an output cap.

use crate::spawn::{ecp_command, run_with_timeout};
use crate::tools::{reserved_token, DemoTool};
use ecp_mcp::spawn::build_argv;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub struct Runner {
    bin: PathBuf,
    timeout: Duration,
    queue_wait: Duration,
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
    /// The argv carries a flag the server owns (`--graph`, `--repo`, `--batch`).
    Reserved(&'static str),
    /// No permit came free within the queue wait.
    Busy,
    Spawn(std::io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::BadArgs(msg) => write!(f, "invalid args: {msg}"),
            RunError::Reserved(flag) => write!(f, "`{flag}` is set by the server"),
            RunError::Busy => write!(f, "busy: every query slot is taken, retry in a few seconds"),
            RunError::Spawn(e) => write!(f, "spawning ecp: {e}"),
        }
    }
}

impl Runner {
    pub fn new(
        bin: PathBuf,
        timeout: Duration,
        queue_wait: Duration,
        max_output_bytes: usize,
        concurrency: usize,
    ) -> Self {
        Self {
            bin,
            timeout,
            queue_wait,
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
        // The caller's tokens are checked as clap will see them: a key such
        // as `Graph` kebab-cases to `--graph`, and a positional value may
        // itself be `--graph=/etc/shadow`; both reach the global flag.
        let caller_argv =
            build_argv(&tool.inner, args).map_err(|e| RunError::BadArgs(e.to_string()))?;
        if let Some(flag) = reserved_token(&caller_argv) {
            return Err(RunError::Reserved(flag));
        }
        let mut argv = vec![tool.inner.subcommand.clone()];
        if tool.takes_repo {
            argv.push("--repo".into());
            argv.push(corpus.display().to_string());
        }
        argv.extend(caller_argv);

        // A closed semaphore is impossible here: the runner owns it for its whole life.
        let _permit = tokio::time::timeout(self.queue_wait, self.permits.acquire())
            .await
            .map_err(|_| RunError::Busy)?
            .expect("runner semaphore stays open");
        let started = Instant::now();
        let mut cmd = ecp_command(&self.bin);
        cmd.args(&argv).current_dir(corpus);

        let mut display_argv = Vec::with_capacity(argv.len() + 1);
        display_argv.push("ecp".to_string());
        display_argv.extend(argv);

        match run_with_timeout(cmd, self.timeout).await {
            Ok(Some(output)) => {
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
            Ok(None) => Ok(Outcome {
                argv: display_argv,
                exit_code: None,
                stdout: String::new(),
                stderr: format!("killed after {}s", self.timeout.as_secs()),
                truncated: false,
                timed_out: true,
                elapsed_ms: started.elapsed().as_millis() as u64,
            }),
            Err(e) => Err(RunError::Spawn(e)),
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
