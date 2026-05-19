//! `cgn admin sessions list` — inspect L1 sessions under all repos.
//! reset / sweep variants deferred (parent spec §11.2 follow-up).

use clap::{Args, Subcommand};
use cgn_core::registry::resolve_home_cgn;
use cgn_core::session::{SessionMeta, SessionState};
use std::fs;
use std::io;

#[derive(Subcommand, Debug)]
pub enum SessionsCommand {
    /// List active L1 sessions across all repos under ~/.cgn/
    List(ListArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Emit JSON instead of the human table.
    #[arg(long, default_value_t = false)]
    pub json: bool,
}

pub fn run(cmd: SessionsCommand) -> Result<(), String> {
    match cmd {
        SessionsCommand::List(args) => run_list(args).map_err(|e| e.to_string()),
    }
}

#[derive(serde::Serialize)]
struct ListRow {
    session_id: String,
    repo: String,
    base_sha: String,
    state: StateView,
    last_touched: String,
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StateView {
    PureReference {
        l2_dirname: String,
    },
    AugmentedReference {
        l2_dirname: String,
        fragment_count: usize,
    },
    Stale {
        reason: String,
    },
}

fn run_list(args: ListArgs) -> io::Result<()> {
    let home_cgn = resolve_home_cgn();
    let rows = collect_rows(&home_cgn)?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&rows).map_err(io::Error::other)?
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!("(no sessions)");
        return Ok(());
    }
    println!(
        "{:<24} {:<24} {:<8} {:<22} {}",
        "SESSION", "REPO", "BASE_SHA", "STATE", "LAST_TOUCHED"
    );
    for r in &rows {
        let state_text = match &r.state {
            StateView::PureReference { .. } => "PureReference".to_string(),
            StateView::AugmentedReference { fragment_count, .. } => {
                format!("Augmented ({fragment_count})")
            }
            StateView::Stale { reason } => format!("Stale({reason})"),
        };
        let base = if r.base_sha.is_empty() {
            "--------".to_string()
        } else {
            r.base_sha[..8.min(r.base_sha.len())].to_string()
        };
        println!(
            "{:<24} {:<24} {:<8} {:<22} {}",
            r.session_id, r.repo, base, state_text, r.last_touched
        );
    }
    Ok(())
}

fn collect_rows(home_cgn: &std::path::Path) -> io::Result<Vec<ListRow>> {
    let mut out = vec![];
    if !home_cgn.exists() {
        return Ok(out);
    }
    for repo_entry in fs::read_dir(home_cgn)? {
        let repo_entry = repo_entry?;
        let repo_dir = repo_entry.path();
        if !repo_dir.is_dir() {
            continue;
        }
        let repo_name = match repo_dir.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let sessions_dir = repo_dir.join("sessions");
        if !sessions_dir.exists() {
            continue;
        }
        // Scan CommitIndex once per repo; every session classification reuses it
        // instead of re-walking commits/ per session. scan_cached reuses across
        // repeated `admin sessions list` calls when commits/ hasn't changed.
        let idx = crate::commit_lookup::CommitIndex::scan_cached(&repo_dir.join("commits")).ok();
        let idx_ref = idx.as_deref();
        for s_entry in fs::read_dir(&sessions_dir)? {
            let s_entry = s_entry?;
            let s_path = s_entry.path();
            if !s_path.is_dir() {
                continue;
            }
            let sid = match s_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if sid.starts_with('.') || sid.contains(".stale-") || sid.contains(".dead") {
                continue;
            }
            // Read SessionMeta once and thread it through both classify and
            // the (base_sha / last_touched) extraction below. classify_with_meta
            // avoids the second session_meta.json open that classify() would
            // otherwise do.
            let sm = SessionMeta::read(&s_path.join("session_meta.json")).ok();
            let state = match &sm {
                Some(sm) => crate::session::state::classify_with_meta(
                    &repo_dir, sid, sm, idx_ref,
                ),
                None => SessionState::Stale {
                    reason: cgn_core::session::StaleReason::MetaUnreadable,
                },
            };
            let (base_sha, state_view) = match &state {
                SessionState::PureReference { base_sha, l2_dirname } => (
                    base_sha.clone(),
                    StateView::PureReference {
                        l2_dirname: l2_dirname.clone(),
                    },
                ),
                SessionState::AugmentedReference {
                    base_sha,
                    l2_dirname,
                    fragment_count,
                } => (
                    base_sha.clone(),
                    StateView::AugmentedReference {
                        l2_dirname: l2_dirname.clone(),
                        fragment_count: *fragment_count,
                    },
                ),
                SessionState::Stale { reason } => (
                    sm.as_ref().map(|m| m.base_sha.clone()).unwrap_or_default(),
                    StateView::Stale {
                        reason: reason.short().to_string(),
                    },
                ),
            };
            let last_touched = sm
                .map(|m| m.last_touched)
                .unwrap_or_else(|| "?".to_string());
            out.push(ListRow {
                session_id: sid.to_string(),
                repo: repo_name.clone(),
                base_sha,
                state: state_view,
                last_touched,
            });
        }
    }
    Ok(out)
}
