//! Ƀ messaging — say / inbox / thread.

use chrono::Utc;
use ecp_core::peer::inbox::{append_entry, InboxEntry};
use ecp_core::peer::registry::alive_peers;
use std::io::Write;
use std::path::Path;
use uuid::Uuid;

/// Resolve a `--to` / `diff` target: exact session-dir id first, else unique
/// agent_name among alive peers. Ambiguity is an error, never a guess.
pub(crate) fn resolve_target_session(
    repo_root: &Path,
    me: &str,
    target: &str,
) -> std::io::Result<String> {
    if repo_root.join("sessions").join(target).is_dir() {
        return Ok(target.to_string());
    }
    let matches: Vec<String> = alive_peers(repo_root, me)
        .into_iter()
        .filter(|p| p.agent_name.as_deref() == Some(target))
        .map(|p| p.session_id)
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no peer session or agent named '{target}'"),
        )),
        many => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "agent name '{target}' is ambiguous: sessions {}",
                many.join(", ")
            ),
        )),
    }
}

pub fn cmd_say(
    repo_root: &Path,
    body: &str,
    to: Option<&str>,
    reply: Option<&str>,
) -> std::io::Result<()> {
    let me = crate::session::resolver::resolve_session_id(None);
    let to_sid = to
        .map(|t| resolve_target_session(repo_root, &me, t))
        .transpose()?;
    let msg_id = format!("m_{}", &Uuid::now_v7().simple().to_string()[..12]);
    let ts = Utc::now().to_rfc3339();
    let from_name = ecp_core::session::SessionMeta::read(
        &repo_root
            .join("sessions")
            .join(&me)
            .join("session_meta.json"),
    )
    .ok()
    .and_then(|m| m.agent_name);
    let entry = InboxEntry::Message {
        ts: ts.clone(),
        msg_id: msg_id.clone(),
        from: me.clone(),
        from_name: from_name.clone(),
        to: to_sid.clone(),
        reply_to: reply.map(|s| s.to_string()),
        body: body.to_string(),
    };

    if let Some(target) = to_sid.as_deref() {
        let inbox = repo_root.join("sessions").join(target).join("inbox.jsonl");
        append_entry(&inbox, &entry)?;
    } else {
        for p in alive_peers(repo_root, &me) {
            let inbox = repo_root
                .join("sessions")
                .join(&p.session_id)
                .join("inbox.jsonl");
            append_entry(&inbox, &entry)?;
        }
    }

    // Always log to my own msg.log with direction=sent
    let msg_log = repo_root.join("sessions").join(&me).join("msg.log");
    if let Some(parent) = msg_log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log_entry = serde_json::json!({
        "ts": ts,
        "direction": "sent",
        "msg_id": msg_id,
        "from": me,
        "from_name": from_name,
        "to": to_sid,
        "reply_to": reply,
        "body": body,
    });
    let line = format!("{log_entry}\n");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&msg_log)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// This is the recovery path behind every truncated hook payload, so it shows
/// the MOST RECENT  entries, not the oldest: the payload drops what did
/// not fit at the end, which is exactly what an oldest-first window hid. It
/// also reads back through the rotated generations, since rotation moves the
/// whole file aside and the entries a payload just dropped can land there.
pub fn cmd_inbox(repo_root: &Path, limit: usize) -> std::io::Result<()> {
    let inbox = repo_root
        .join("sessions")
        .join(crate::session::resolver::resolve_session_id(None))
        .join("inbox.jsonl");

    // Oldest generation first so the concatenation stays chronological.
    let mut lines: Vec<String> = Vec::new();
    let mut generations: Vec<std::path::PathBuf> = (1
        ..=ecp_core::peer::retention::INBOX_KEEP_ROTATED)
        .map(|n| inbox.with_file_name(format!("inbox.jsonl.{n}")))
        .filter(|p| p.exists())
        .collect();
    generations.reverse();
    generations.push(inbox.clone());
    for path in &generations {
        if let Ok(content) = std::fs::read_to_string(path) {
            lines.extend(content.lines().map(str::to_string));
        }
    }

    if lines.is_empty() {
        println!("inbox empty");
        return Ok(());
    }
    let skipped = lines.len().saturating_sub(limit);
    if skipped > 0 {
        println!(
            "... {skipped} older entr{} not shown (raise --limit)",
            if skipped == 1 { "y" } else { "ies" }
        );
    }
    for line in lines.iter().skip(skipped) {
        println!("{line}");
    }
    Ok(())
}

pub fn cmd_thread(repo_root: &Path, msg_id: &str) -> std::io::Result<()> {
    let me = crate::session::resolver::resolve_session_id(None);
    let msg_log = repo_root.join("sessions").join(&me).join("msg.log");
    let Ok(content) = std::fs::read_to_string(&msg_log) else {
        println!("no messages");
        return Ok(());
    };
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let mid = v.get("msg_id").and_then(|x| x.as_str()).unwrap_or("");
        let reply = v.get("reply_to").and_then(|x| x.as_str()).unwrap_or("");
        if mid == msg_id || reply == msg_id {
            println!("{line}");
        }
    }
    Ok(())
}
