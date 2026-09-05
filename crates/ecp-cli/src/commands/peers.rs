//! `ecp peers` CLI surface.

use crate::session::resolver::resolve_session_id;
use clap::{Args, Subcommand};
use ecp_core::peer::registry::alive_peers;
use ecp_core::EcpError;
use std::path::PathBuf;

fn default_repo_root() -> std::io::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let repo_dir = crate::repo_identity::repo_dir_name_for_cwd(&cwd)?;
    Ok(ecp_core::registry::resolve_home_ecp().join(repo_dir))
}

#[derive(Args, Debug, Clone)]
pub struct PeersArgs {
    #[command(subcommand)]
    pub cmd: PeersCmd,
    #[arg(long, global = true)]
    pub repo: Option<PathBuf>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum PeersCmd {
    /// List alive peer sessions
    Status {
        #[arg(long, value_enum, default_value_t = StatusFormat::Text)]
        format: StatusFormat,
        /// Lead overview: pairwise HARD overlap (shared dirty files) across ALL
        /// alive sessions (own session included), instead of the per-session list
        #[arg(long, default_value_t = false)]
        pairs: bool,
    },
    /// Show a peer's symbol-level dirty surface (optionally filtered by symbol)
    Diff {
        peer: String,
        symbol: Option<String>,
    },
    /// Tail this session's msg.log (Ƀ messaging history)
    Log {
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        peer: Option<String>,
        #[arg(long)]
        direction: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Ƀ Send a message (broadcast or targeted, fire-and-forget)
    Say {
        body: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        reply: Option<String>,
    },
    /// Ƀ Read the inbox in full without consuming it. This is where content
    /// the 4 KB hook payload could not fit stays queued — nothing is ever
    /// discarded automatically, so `--clear` is the only way to drop it.
    Inbox {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Discard everything in the inbox, including entries never shown.
        #[arg(long, default_value_t = false)]
        clear: bool,
    },
    /// Ƀ Print message thread by msg_id (current session msg.log)
    Thread { msg_id: String },
    /// Set this session's agent name (shown in status / payloads, targetable by say --to)
    Name { name: String },
    /// Pre-spawn disjointness check: do these targets' blast radii collide?
    /// Operates on the repo at cwd (not the peers data dir).
    Plan {
        /// Symbols the lead intends to split across agents (comma-separated)
        #[arg(long, value_delimiter = ',', required = true)]
        targets: Vec<String>,
        /// Impact direction per target: a work package collides if either
        /// caller or callee chains intersect, so `both` is the safe default
        #[arg(long, default_value = "both", value_parser = ["upstream", "up", "downstream", "down", "both"])]
        direction: String,
        /// Max impact traversal depth per target
        #[arg(long, default_value_t = 5)]
        depth: u32,
        /// Count test-only symbols as collisions
        #[arg(long, default_value_t = false)]
        include_tests: bool,
        #[arg(long, value_enum, default_value_t = StatusFormat::Text)]
        format: StatusFormat,
    },
    /// Start, stop, or check the inotify-driven peer-dirty watcher daemon.
    Watch(super::watch::WatchArgs),
    /// Rotate logs + cleanup
    Gc,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum StatusFormat {
    Text,
    Json,
}

pub fn run(args: PeersArgs) -> std::io::Result<()> {
    let repo_root = match args.repo.clone() {
        Some(p) => p,
        None => default_repo_root()?,
    };
    match args.cmd {
        PeersCmd::Status { format, pairs } => {
            if pairs {
                cmd_status_pairs(&repo_root, format)
            } else {
                cmd_status(&repo_root, format)
            }
        }
        PeersCmd::Diff { peer, symbol } => cmd_diff(&repo_root, &peer, symbol.as_deref()),
        PeersCmd::Log {
            since,
            peer,
            direction,
            limit,
        } => cmd_log(
            &repo_root,
            since.as_deref(),
            peer.as_deref(),
            direction.as_deref(),
            limit,
        ),
        PeersCmd::Say { body, to, reply } => {
            super::peers_msg::cmd_say(&repo_root, &body, to.as_deref(), reply.as_deref())
        }
        PeersCmd::Inbox { limit, clear } => super::peers_msg::cmd_inbox(&repo_root, limit, clear),
        PeersCmd::Name { name } => cmd_name(&repo_root, &name),
        PeersCmd::Plan {
            targets,
            direction,
            depth,
            include_tests,
            format,
        } => super::peers_plan::cmd_plan(
            &targets,
            &direction,
            depth,
            include_tests,
            matches!(format, StatusFormat::Json),
        ),
        PeersCmd::Thread { msg_id } => super::peers_msg::cmd_thread(&repo_root, &msg_id),
        PeersCmd::Watch(wargs) => super::watch::run(wargs).map_err(io_from_ecp),
        PeersCmd::Gc => cmd_gc(&repo_root),
    }
}

fn io_from_ecp(e: EcpError) -> std::io::Error {
    match e {
        EcpError::Io(io) => io,
        other => std::io::Error::other(other.to_string()),
    }
}

/// Three-state classification of watcher liveness for status output.
/// `not-started`: session_meta.watcher_pid is None — `ecp peers watch --start`
/// was never invoked. `dead`: pid recorded but the OS process is gone (crashed
/// or stale). `alive`: pid recorded and reachable.
fn watcher_state(p: &ecp_core::peer::registry::PeerSession) -> &'static str {
    match (p.watcher_pid, p.watcher_alive) {
        (None, _) => "not-started",
        (Some(_), false) => "dead",
        (Some(_), true) => "alive",
    }
}

fn cmd_status(repo_root: &std::path::Path, format: StatusFormat) -> std::io::Result<()> {
    let me = resolve_session_id(None);
    let peers = alive_peers(repo_root, &me);
    match format {
        StatusFormat::Text => {
            if peers.is_empty() {
                println!("no peers");
                return Ok(());
            }
            for p in &peers {
                println!(
                    "session={}\tname={}\tpid={}\tlast_touched={}\twatcher={}",
                    p.session_id,
                    p.agent_name.as_deref().unwrap_or("-"),
                    p.pid.map_or_else(|| "-".to_string(), |v| v.to_string()),
                    p.last_touched,
                    watcher_state(p)
                );
            }
        }
        StatusFormat::Json => {
            let rows: Vec<_> = peers
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "session_id": p.session_id,
                        "agent_name": p.agent_name,
                        "pid": p.pid,
                        "last_touched": p.last_touched.to_rfc3339(),
                        "base_sha": p.base_sha,
                        "watcher": watcher_state(p),
                        "watcher_pid": p.watcher_pid,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string(&rows).map_err(std::io::Error::other)?
            );
        }
    }
    Ok(())
}

/// Pairwise HARD-overlap matrix across all alive sessions — the lead's
/// global view. Reuses `concern::classify`'s HARD definition: a shared dirty
/// FILE. The manifest lists every declaration of a re-parsed file and never
/// which ones changed, so intersecting declarations would claim a shared edit
/// nobody recorded.
///
/// SOFT (impact-neighbor) pairs are not computed here: that needs the graph
/// engine, and an honest omission beats an empty column (`peers plan` covers
/// the proactive impact-level question).
fn cmd_status_pairs(repo_root: &std::path::Path, format: StatusFormat) -> std::io::Result<()> {
    use ecp_core::session::overlay::DirtyFiles;
    use std::collections::HashSet;

    // Empty exclude id matches no session dir → own session included.
    let sessions = alive_peers(repo_root, "");
    let dirty: Vec<HashSet<String>> = sessions
        .iter()
        .map(|s| {
            DirtyFiles::read(
                &repo_root
                    .join("sessions")
                    .join(&s.session_id)
                    .join("dirty_files.json"),
            )
            .map(|d| d.entries.into_keys().collect())
            .unwrap_or_default()
        })
        .collect();

    const FILES_CAP: usize = 10;
    let label = |i: usize| -> String {
        match sessions[i].agent_name.as_deref() {
            Some(n) => format!("{}({n})", sessions[i].session_id),
            None => sessions[i].session_id.clone(),
        }
    };
    let mut rows: Vec<(usize, usize, Vec<String>)> = Vec::new();
    for i in 0..sessions.len() {
        for j in (i + 1)..sessions.len() {
            let mut shared: Vec<String> = dirty[i].intersection(&dirty[j]).cloned().collect();
            if shared.is_empty() {
                continue;
            }
            shared.sort();
            rows.push((i, j, shared));
        }
    }
    match format {
        StatusFormat::Text => {
            println!(
                "pairs: {} session{}, {} overlapping pair{}",
                sessions.len(),
                if sessions.len() == 1 { "" } else { "s" },
                rows.len(),
                if rows.len() == 1 { "" } else { "s" }
            );
            if rows.is_empty() {
                println!("no overlapping pairs — no two sessions share a dirty file");
                return Ok(());
            }
            for (i, j, shared) in &rows {
                let files: Vec<&str> = shared.iter().take(FILES_CAP).map(|s| s.as_str()).collect();
                println!(
                    "{} ↔ {}: HARD {} shared dirty file{} — {}",
                    label(*i),
                    label(*j),
                    shared.len(),
                    if shared.len() == 1 { "" } else { "s" },
                    files.join(", ")
                );
            }
        }
        StatusFormat::Json => {
            let payload = serde_json::json!({
                "sessions": sessions.iter().map(|s| serde_json::json!({
                    "session_id": s.session_id,
                    "agent_name": s.agent_name,
                })).collect::<Vec<_>>(),
                "pairs": rows.iter().map(|(i, j, shared)| serde_json::json!({
                    "a": sessions[*i].session_id,
                    "a_name": sessions[*i].agent_name,
                    "b": sessions[*j].session_id,
                    "b_name": sessions[*j].agent_name,
                    "hard_count": shared.len(),
                    "files": shared.iter().take(FILES_CAP).collect::<Vec<_>>(),
                    "files_capped": shared.len() > FILES_CAP,
                })).collect::<Vec<_>>(),
            });
            println!(
                "{}",
                serde_json::to_string(&payload).map_err(std::io::Error::other)?
            );
        }
    }
    Ok(())
}

fn cmd_name(repo_root: &std::path::Path, name: &str) -> std::io::Result<()> {
    let me = resolve_session_id(None);
    ecp_core::registry::validate_cache_component(&me)?;
    let meta_path = repo_root
        .join("sessions")
        .join(&me)
        .join("session_meta.json");
    let mut meta = ecp_core::session::SessionMeta::read(&meta_path).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("session '{me}' not enrolled yet (run any ecp query in this repo first): {e}"),
        )
    })?;
    meta.agent_name = Some(name.to_string());
    ecp_core::session::SessionMeta::write_atomic(&meta_path, &meta)?;
    println!("session={me} name={name}");
    Ok(())
}

fn cmd_diff(repo_root: &std::path::Path, peer: &str, symbol: Option<&str>) -> std::io::Result<()> {
    use ecp_core::session::overlay::DirtyFiles;
    let me = resolve_session_id(None);
    let peer = super::peers_msg::resolve_target_session(repo_root, &me, peer)?;
    let path = repo_root
        .join("sessions")
        .join(&peer)
        .join("dirty_files.json");
    let peer_dirty = DirtyFiles::read(&path)?;
    for (path_key, entry) in &peer_dirty.entries {
        if let Some(sym) = symbol {
            if !entry.dirty_symbols.iter().any(|s| s.name == sym) {
                continue;
            }
        }
        println!("--- {path_key} ---");
        for s in &entry.dirty_symbols {
            println!(
                "  {} ({:?}, L{}-{})",
                s.name, s.kind, s.line_start, s.line_end
            );
        }
    }
    Ok(())
}

fn cmd_log(
    repo_root: &std::path::Path,
    _since: Option<&str>,
    peer: Option<&str>,
    direction: Option<&str>,
    limit: usize,
) -> std::io::Result<()> {
    let me = resolve_session_id(None);
    ecp_core::registry::validate_cache_component(&me)?;
    let msg_log = repo_root.join("sessions").join(&me).join("msg.log");
    let Ok(content) = std::fs::read_to_string(&msg_log) else {
        println!("no messages");
        return Ok(());
    };
    let mut printed = 0;
    for line in content.lines().rev() {
        if printed >= limit {
            break;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(p) = peer {
                let from = v.get("from").and_then(|x| x.as_str()).unwrap_or("");
                let to = v.get("to").and_then(|x| x.as_str()).unwrap_or("");
                if from != p && to != p {
                    continue;
                }
            }
            if let Some(d) = direction {
                let dir = v.get("direction").and_then(|x| x.as_str()).unwrap_or("");
                if dir != d {
                    continue;
                }
            }
            println!("{line}");
            printed += 1;
        }
    }
    if printed == 0 {
        println!("no messages");
    }
    Ok(())
}

fn cmd_gc(repo_root: &std::path::Path) -> std::io::Result<()> {
    use ecp_core::peer::retention::*;
    let me = resolve_session_id(None);
    ecp_core::registry::validate_cache_component(&me)?;
    let session_dir = repo_root.join("sessions").join(&me);
    let _ = rotate_if_needed(
        &session_dir.join("msg.log"),
        MSG_LOG_ROTATE_BYTES,
        MSG_LOG_KEEP_ROTATED,
    );
    let _ = rotate_if_needed(
        &session_dir.join("watcher.log"),
        WATCHER_LOG_ROTATE_BYTES,
        WATCHER_LOG_KEEP_ROTATED,
    );
    println!("rotated logs for session={me}");
    Ok(())
}
