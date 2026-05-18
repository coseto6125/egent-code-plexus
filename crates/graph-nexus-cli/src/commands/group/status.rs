//! `gnx group status <name>` — compare each group member's current HEAD
//! against the last-synced snapshot in `meta.json`.

use clap::Args;
use graph_nexus_core::registry::{resolve_home_gnx, RegistryFile};
use graph_nexus_core::GnxError;
use std::path::Path;

use crate::commands::group::lookup_member;
use crate::commands::group::storage::{self, GroupMeta};

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Group name (must exist in registry.json).
    pub name: String,
    /// Emit JSON instead of TOON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug)]
enum MemberStatus {
    Ok,
    Stale { commits_behind: Option<u64> },
    Missing,
    NoSnapshot,
    NoMeta,
}

struct MemberReport {
    name: String,
    status: MemberStatus,
}

pub fn run(args: StatusArgs) -> Result<(), GnxError> {
    let home_gnx = resolve_home_gnx();
    let registry_path = home_gnx.join("registry.json");
    let reg = RegistryFile::read_or_empty(&registry_path)?;

    let group_entry = reg
        .groups
        .iter()
        .find(|g| g.name == args.name)
        .ok_or_else(|| {
            GnxError::InvalidArgument(format!(
                "group '{}' not found in registry\n\
                 → create it with `gnx admin group add <repo> {}`",
                args.name, args.name
            ))
        })?
        .clone();

    let group_dir = storage::group_dir(&home_gnx, &args.name);
    let meta_path = group_dir.join(storage::META_FILE);

    // No meta.json → never synced; all members are NO_META.
    if !meta_path.exists() {
        let reports: Vec<MemberReport> = group_entry
            .members
            .iter()
            .map(|m| MemberReport { name: m.clone(), status: MemberStatus::NoMeta })
            .collect();
        emit(&args.name, None, &reports, args.json);
        return Ok(());
    }

    let meta = storage::read_meta(&group_dir).map_err(GnxError::Io)?;

    let reports: Vec<MemberReport> = group_entry
        .members
        .iter()
        .map(|member| {
            let status = resolve_member_status(member, &reg, &meta);
            MemberReport { name: member.clone(), status }
        })
        .collect();

    emit(&args.name, Some(&meta.generated_at), &reports, args.json);
    Ok(())
}

fn resolve_member_status(member: &str, reg: &RegistryFile, meta: &GroupMeta) -> MemberStatus {
    let Some(alias) = lookup_member(reg, member) else {
        return MemberStatus::Missing;
    };

    let common_dir = std::path::PathBuf::from(&alias.common_dir);
    let repo_root = match common_dir.parent() {
        Some(p) => p.to_path_buf(),
        None => common_dir.clone(),
    };

    let Some(snapshot) = meta.repo_snapshots.get(member) else {
        return MemberStatus::NoSnapshot;
    };

    let Some(head) = git_head(&repo_root) else {
        return MemberStatus::NoSnapshot;
    };

    if head == snapshot.last_commit {
        return MemberStatus::Ok;
    }

    let commits_behind = git_commits_behind(&repo_root, &snapshot.last_commit);
    MemberStatus::Stale { commits_behind }
}

fn git_head(repo_root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(
        std::str::from_utf8(&out.stdout)
            .unwrap_or("")
            .trim()
            .to_string(),
    )
}

fn git_commits_behind(repo_root: &Path, stored_commit: &str) -> Option<u64> {
    let range = format!("{stored_commit}..HEAD");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-list", "--count", &range])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    std::str::from_utf8(&out.stdout)
        .unwrap_or("")
        .trim()
        .parse::<u64>()
        .ok()
}

fn emit(name: &str, last_sync: Option<&str>, reports: &[MemberReport], json: bool) {
    if json {
        emit_json(name, last_sync, reports);
    } else {
        emit_toon(name, last_sync, reports);
    }
}

fn emit_toon(name: &str, last_sync: Option<&str>, reports: &[MemberReport]) {
    match last_sync {
        Some(ts) => println!("Group: {name} (last sync: {ts})"),
        None => println!("Group: {name} (never synced)"),
    }
    for r in reports {
        let padded = format!("{:<25}", r.name);
        let status_str = match &r.status {
            MemberStatus::Ok => "OK".to_string(),
            MemberStatus::Stale { commits_behind } => match commits_behind {
                Some(n) => format!("STALE   ({n} behind)"),
                None => "STALE   (? behind)".to_string(),
            },
            MemberStatus::Missing => "MISSING".to_string(),
            MemberStatus::NoSnapshot => "NO_SNAPSHOT".to_string(),
            MemberStatus::NoMeta => "NO_META".to_string(),
        };
        println!("  {padded}{status_str}");
    }
}

fn emit_json(name: &str, last_sync: Option<&str>, reports: &[MemberReport]) {
    let last_sync_field = match last_sync {
        Some(ts) => format!(r#""{ts}""#),
        None => "null".to_string(),
    };
    let members_arr: Vec<String> = reports
        .iter()
        .map(|r| {
            let name_esc = r.name.replace('"', "\\\"");
            match &r.status {
                MemberStatus::Ok => {
                    format!(r#"{{"name":"{name_esc}","status":"OK"}}"#)
                }
                MemberStatus::Stale { commits_behind } => match commits_behind {
                    Some(n) => format!(
                        r#"{{"name":"{name_esc}","status":"STALE","commits_behind":{n}}}"#
                    ),
                    None => format!(r#"{{"name":"{name_esc}","status":"STALE"}}"#),
                },
                MemberStatus::Missing => {
                    format!(r#"{{"name":"{name_esc}","status":"MISSING"}}"#)
                }
                MemberStatus::NoSnapshot => {
                    format!(r#"{{"name":"{name_esc}","status":"NO_SNAPSHOT"}}"#)
                }
                MemberStatus::NoMeta => {
                    format!(r#"{{"name":"{name_esc}","status":"NO_META"}}"#)
                }
            }
        })
        .collect();
    println!(
        r#"{{"group":"{name}","last_sync":{last_sync_field},"members":[{}]}}"#,
        members_arr.join(",")
    );
}
