//! L1 fragment writer: re-parses a dirty source file, archives the per-file
//! graph fragment via rkyv, updates `dirty_files.json` + `session_meta.overlay_version`.
//!
//! Atomic semantics: each fragment is written to `<id>.tmp` → fsync → rename,
//! so a reader merging fragments can never see partial state. The manifest
//! rewrite happens after the fragment rename, in the same `atomic_write_json`
//! style — readers always see a consistent snapshot.

use graph_nexus_core::registry::atomic_write_json;
use graph_nexus_core::session::{DirtyEntry, DirtyFiles, SessionMeta};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::Path;

pub struct FragmentInput {
    pub rel_path: String,
    pub content: Vec<u8>,
    pub mtime_ns: u64,
}

pub struct FragmentOutcome {
    // Asserted by `tests/overlay_writer.rs` + `tests/promotion.rs`; bin path
    // currently only branches on `parse_failed`.
    #[allow(dead_code)]
    pub fragment_id: String,
    pub parse_failed: bool,
}

/// Write or update a dirty-file fragment for the given session.
///
/// Pre-conditions: `session_dir` exists and contains a valid
/// `session_meta.json`. Caller is responsible for creating the session
/// dir + initial session meta — see auto_ensure / promotion paths.
pub fn write_dirty_fragment(
    session_dir: &Path,
    input: &FragmentInput,
) -> io::Result<FragmentOutcome> {
    let content_hash = sha256_hex(&input.content);
    let fragment_id = content_hash[..16].to_string();

    let overlay_dir = session_dir.join("graph_overlay");
    fs::create_dir_all(&overlay_dir)?;

    let fragment_path = overlay_dir.join(format!("{fragment_id}.bin"));

    // Parse the file content. On failure we keep any prior fragment intact
    // and just mark `parse_failed: true` in the manifest — queries on that
    // file still get the stale-but-valid prior fragment, not a hard error.
    let archive_bytes = match parse_to_fragment(&input.rel_path, &input.content) {
        Ok(b) => b,
        Err(_) => {
            update_manifest(
                session_dir,
                &input.rel_path,
                &fragment_id,
                &content_hash,
                input.mtime_ns,
                true,
            )?;
            return Ok(FragmentOutcome {
                fragment_id,
                parse_failed: true,
            });
        }
    };

    // Atomic write: tmp → fsync → rename
    let tmp = overlay_dir.join(format!("{fragment_id}.tmp"));
    fs::write(&tmp, &archive_bytes)?;
    let f = fs::File::open(&tmp)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, &fragment_path)?;

    update_manifest(
        session_dir,
        &input.rel_path,
        &fragment_id,
        &content_hash,
        input.mtime_ns,
        false,
    )?;
    bump_overlay_version(session_dir)?;

    Ok(FragmentOutcome {
        fragment_id,
        parse_failed: false,
    })
}

fn parse_to_fragment(rel_path: &str, content: &[u8]) -> io::Result<Vec<u8>> {
    crate::commands::scan::parse_single_file_to_fragment(rel_path, content)
}

fn update_manifest(
    session_dir: &Path,
    rel_path: &str,
    fragment_id: &str,
    content_hash: &str,
    mtime_ns: u64,
    parse_failed: bool,
) -> io::Result<()> {
    let manifest_path = session_dir.join("dirty_files.json");
    let mut df = if manifest_path.exists() {
        DirtyFiles::read(&manifest_path)?
    } else {
        DirtyFiles::empty()
    };
    df.entries.insert(
        rel_path.to_string(),
        DirtyEntry {
            mtime_ns,
            content_hash: content_hash.to_string(),
            fragment_id: fragment_id.to_string(),
            tantivy_delta_segment: None,
            parse_failed,
        },
    );
    atomic_write_json(&manifest_path, &df)
}

fn bump_overlay_version(session_dir: &Path) -> io::Result<()> {
    let path = session_dir.join("session_meta.json");
    let mut sm = SessionMeta::read(&path)?;
    sm.overlay_version += 1;
    sm.last_touched = chrono::Utc::now().to_rfc3339();
    atomic_write_json(&path, &sm)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
