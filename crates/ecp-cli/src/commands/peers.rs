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
    /// Ƀ Inspect inbox without draining (debug)
    Inbox {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Ƀ Print message thread by msg_id (current session msg.log)
    Thread { msg_id: String },
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
        PeersCmd::Status { format } => cmd_status(&repo_root, format),
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
        PeersCmd::Inbox { limit } => super::peers_msg::cmd_inbox(&repo_root, limit),
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
                    "session={}\tpid={}\tlast_touched={}\twatcher={}",
                    p.session_id,
                    p.pid,
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

fn cmd_diff(repo_root: &std::path::Path, peer: &str, symbol: Option<&str>) -> std::io::Result<()> {
    use ecp_core::session::overlay::DirtyFiles;
    let path = repo_root
        .join("sessions")
        .join(peer)
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
