//! `ecp-demo`: a public, read-only web front for the `ecp` CLI.
//!
//! A visitor pastes a public GitHub repository; the service clones it,
//! indexes it with `ecp admin index`, and from then on every request spawns
//! `ecp <subcommand> --repo <checkout> …` with the argv the MCP server would
//! build. The tool list, its JSON schemas and the JSON→argv translation come
//! from `ecp-mcp`, so the page shows exactly the surface an agent gets.
//! The service adds only what a public endpoint needs: a subcommand
//! allowlist, server-owned flags, per-run timeouts, an output cap, one build
//! at a time, size and count ceilings on the checkouts, and per-IP rate limits.

pub mod app;
pub mod ratelimit;
pub mod repos;
pub mod runner;
pub mod spawn;
pub mod tools;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;

/// Runtime knobs, all from the environment so one image serves every host.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub bin: PathBuf,
    pub git: PathBuf,
    pub curl: PathBuf,
    pub repos_dir: PathBuf,
    pub timeout: Duration,
    pub queue_wait: Duration,
    pub max_output_bytes: usize,
    pub rate_per_min: u32,
    pub add_rate_per_hour: u32,
    pub concurrency: usize,
    /// How many `x-forwarded-for` hops were appended by proxies this
    /// deployment trusts; 0 ignores the header and uses the socket peer.
    pub trusted_hops: usize,
    pub max_repo_kb: u64,
    pub max_repos: usize,
    pub queue_limit: usize,
    pub clone_timeout: Duration,
    pub index_timeout: Duration,
    pub github_token: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            port: env_or("PORT", 8080)?,
            bin: env_path("ECP_DEMO_BIN", "ecp"),
            git: env_path("ECP_DEMO_GIT", "git"),
            curl: env_path("ECP_DEMO_CURL", "curl"),
            repos_dir: env_path("ECP_DEMO_REPOS", "/data/repos"),
            timeout: Duration::from_secs(env_or("ECP_DEMO_TIMEOUT_SECS", 15)?),
            queue_wait: Duration::from_secs(env_or("ECP_DEMO_QUEUE_WAIT_SECS", 5)?),
            max_output_bytes: env_or("ECP_DEMO_MAX_OUTPUT_BYTES", 256 * 1024)?,
            rate_per_min: env_or("ECP_DEMO_RATE_PER_MIN", 60)?,
            add_rate_per_hour: env_or("ECP_DEMO_ADD_RATE_PER_HOUR", 10)?,
            concurrency: env_or("ECP_DEMO_CONCURRENCY", 2)?,
            trusted_hops: env_or("ECP_DEMO_TRUSTED_HOPS", 1)?,
            max_repo_kb: env_or("ECP_DEMO_MAX_REPO_KB", 50 * 1024)?,
            max_repos: env_or("ECP_DEMO_MAX_REPOS", 6)?,
            queue_limit: env_or("ECP_DEMO_QUEUE_LIMIT", 3)?,
            clone_timeout: Duration::from_secs(env_or("ECP_DEMO_CLONE_TIMEOUT_SECS", 120)?),
            index_timeout: Duration::from_secs(env_or("ECP_DEMO_INDEX_TIMEOUT_SECS", 300)?),
            github_token: std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()),
        })
    }
}

fn env_path(key: &str, default: &str) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(raw) => raw
            .parse()
            .map_err(|e| anyhow::anyhow!("{key}={raw:?}: {e}"))
            .context("invalid environment value"),
        Err(_) => Ok(default),
    }
}
