//! The L1 store of the invalidation closure (`ecp_core::session::closure`):
//! `<session>/l1/closure.bin`.
//!
//! The refresh that writes the dirty fragments (`auto_ensure`) computes the
//! closure over the manifest it leaves behind, re-parses each clean file the
//! closure names, and stores those parses here with what the closure left as
//! indexed ([`StaleClosure`]). The query side reads them as more view inputs
//! ([`attach`]). The manifest (`dirty_events.json`) never lists a closure
//! file, so a peer reading it sees only this session's edits, and neither
//! the retire pass nor `admin gc` touches this file.
//!
//! File layout: key (u64 LE) · rkyv [`ClosureFile`]. The key hashes what the
//! closure was computed from ([`closure_key`]): the session's base SHA and
//! every manifest row. A reader serves the closure only under the key of the
//! manifest it reads, so a closure never meets another dirty state; a
//! refresh that finds its key stored does nothing beyond an 8-byte read.

use super::overlay_reader::{self, AcceptedFragment, ViewInputs};
use super::view_cache::hash_part;
use ecp_core::contracts::routing::SizeCap;
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::registry::BUILDER_FINGERPRINT;
use ecp_core::session::closure::{invalidation_closure, StaleClosure};
use ecp_core::session::{layout, DirtyEvents, SessionMeta};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::Xxh3;

/// The closure file's name under `l1/`.
pub const CLOSURE_FILE: &str = "closure.bin";

/// Layout version of [`ClosureFile`]. A file of another version is no
/// closure: the next refresh rewrites it.
pub const CLOSURE_FORMAT: u32 = 1;

const KEY_LEN: usize = 8;

/// One stored closure.
#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ClosureFile {
    /// [`CLOSURE_FORMAT`] of the writer.
    pub format: u32,
    /// `BUILDER_FINGERPRINT` of the binary that parsed the entries.
    pub builder_fingerprint: String,
    /// `SizeCap::bytes()` of the cap the entries were parsed under.
    pub size_cap_bytes: u64,
    pub stale: StaleClosure,
    /// One per parse of a closure file, in path order.
    pub entries: Vec<ClosureEntry>,
}

/// One clean file's parse.
#[derive(Debug, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ClosureEntry {
    /// `File.path`-normalized.
    pub rel_path: String,
    /// The file's mtime, statted before the parse.
    pub mtime_ns: u64,
    /// The fragment archive the dirty route writes for the same parse
    /// (`overlay_writer::archive_fragments`).
    pub fragment: Vec<u8>,
}

/// `<session>/l1/closure.bin`.
pub fn closure_path(session_dir: &Path) -> PathBuf {
    layout::l1_dir(session_dir).join(CLOSURE_FILE)
}

/// The key of a closure computed over `manifest` for a session based at
/// `base_sha`: every row, in manifest order.
pub fn closure_key(base_sha: &str, manifest: &DirtyEvents) -> u64 {
    let mut hasher = Xxh3::new();
    hash_part(&mut hasher, &CLOSURE_FORMAT.to_le_bytes());
    hash_part(&mut hasher, base_sha.as_bytes());
    hash_part(&mut hasher, manifest.builder_fingerprint.as_bytes());
    hash_part(&mut hasher, &manifest.size_cap_bytes.to_le_bytes());
    for (path, entry) in &manifest.entries {
        hash_part(&mut hasher, path.as_bytes());
        hash_part(
            &mut hasher,
            entry.fragment_id.as_deref().unwrap_or("").as_bytes(),
        );
        hash_part(&mut hasher, &entry.mtime_ns.to_le_bytes());
        hash_part(&mut hasher, &entry.format.to_le_bytes());
        hash_part(&mut hasher, &[u8::from(entry.parse_failed)]);
    }
    hasher.digest()
}

/// The key of the session's current manifest; `None` without a readable
/// session meta.
fn session_key(session_dir: &Path, manifest: &DirtyEvents) -> Option<u64> {
    let meta = SessionMeta::read(&session_dir.join("session_meta.json")).ok()?;
    Some(closure_key(&meta.base_sha, manifest))
}

/// The key the stored closure was written under; `None` without one.
fn stored_key(session_dir: &Path) -> Option<u64> {
    let mut head = [0u8; KEY_LEN];
    File::open(closure_path(session_dir))
        .and_then(|mut file| file.read_exact(&mut head))
        .ok()?;
    Some(u64::from_le_bytes(head))
}

/// The closure stored under `key` by this binary under `size_cap`; `None`
/// for another key, format, builder or cap, and for bytes that fail
/// validation.
fn read(session_dir: &Path, key: u64, size_cap: SizeCap) -> Option<ClosureFile> {
    let bytes = fs::read(closure_path(session_dir)).ok()?;
    let (head, body) = bytes.split_at_checked(KEY_LEN)?;
    if u64::from_le_bytes(head.try_into().ok()?) != key {
        return None;
    }
    // Checked access refuses misaligned input, and a `fs::read` buffer
    // carries no alignment guarantee past the 8-byte key.
    let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(body.len());
    aligned.extend_from_slice(body);
    let file = rkyv::from_bytes::<ClosureFile, rkyv::rancor::Error>(&aligned).ok()?;
    (file.format == CLOSURE_FORMAT
        && file.builder_fingerprint == BUILDER_FINGERPRINT
        && file.size_cap_bytes == size_cap.bytes())
    .then_some(file)
}

fn write(session_dir: &Path, key: u64, file: &ClosureFile) -> io::Result<()> {
    let body = rkyv::to_bytes::<rkyv::rancor::Error>(file).map_err(io::Error::other)?;
    let mut bytes = Vec::with_capacity(KEY_LEN + body.len());
    bytes.extend_from_slice(&key.to_le_bytes());
    bytes.extend_from_slice(&body);
    ecp_core::registry::atomic_write_bytes_no_fsync(&closure_path(session_dir), &bytes)
}

/// Add the closure stored for `manifest` (the current one under
/// `size_cap`) to `inputs`: each entry whose file still has its parsed mtime
/// and is no dirty path, hashed after the dirty inputs, then its stale
/// kinds. A dirty view with no closure stored for this manifest gets
/// `StaleClosure::not_computed`, and so does one whose closure file was
/// touched since its parse: that file is served as indexed.
pub(crate) fn attach(
    session_dir: &Path,
    worktree_root: &Path,
    size_cap: SizeCap,
    manifest: &DirtyEvents,
    inputs: &mut ViewInputs,
) {
    if inputs.is_empty() {
        return;
    }
    let Some(file) =
        session_key(session_dir, manifest).and_then(|key| read(session_dir, key, size_cap))
    else {
        inputs.stale = StaleClosure::not_computed();
        return;
    };
    let mut hasher = Xxh3::new();
    hasher.update(&inputs.hash.to_le_bytes());
    let mut stale = file.stale;
    for entry in file.entries {
        if manifest.entries.contains_key(&entry.rel_path) {
            continue; // the dirty parse of the path wins
        }
        let current = crate::auto_ensure::mtime_ns_of(&worktree_root.join(&entry.rel_path))
            == Some(entry.mtime_ns);
        if !current || !overlay_reader::valid_fragment(&entry.fragment) {
            stale = StaleClosure::not_computed();
            continue;
        }
        hash_part(&mut hasher, entry.rel_path.as_bytes());
        hash_part(&mut hasher, &entry.fragment);
        inputs.fragments.push(AcceptedFragment {
            rel_path: entry.rel_path,
            bytes: entry.fragment,
        });
    }
    hash_part(&mut hasher, format!("{stale:?}").as_bytes());
    inputs.stale = stale;
    inputs.hash = hasher.digest();
}

/// Store the closure of the session's current manifest unless it is stored
/// already: `auto_ensure` calls this after each L1 refresh and retire pass.
/// The graph is loaded only when the stored key is not the manifest's.
pub fn refresh(
    worktree_root: &Path,
    session_dir: &Path,
    graph_path: &Path,
    size_cap: SizeCap,
) -> io::Result<()> {
    let Some(key) = current_key(session_dir, size_cap) else {
        return Ok(());
    };
    if stored_key(session_dir) == Some(key) {
        return Ok(());
    }
    let engine = crate::engine::Engine::load(graph_path)?;
    let graph = engine
        .graph()
        .map_err(|e| io::Error::other(e.to_string()))?;
    store(worktree_root, session_dir, graph, size_cap, key)
}

/// [`refresh`] over a graph the caller holds, whatever key is stored.
pub fn refresh_with_graph(
    worktree_root: &Path,
    session_dir: &Path,
    graph: &ArchivedZeroCopyGraph,
    size_cap: SizeCap,
) -> io::Result<()> {
    match current_key(session_dir, size_cap) {
        Some(key) => store(worktree_root, session_dir, graph, size_cap, key),
        None => Ok(()),
    }
}

/// The key of the current manifest under `size_cap`; `None` without a
/// current manifest or a session meta: the reader then serves no dirty
/// input either.
fn current_key(session_dir: &Path, size_cap: SizeCap) -> Option<u64> {
    let manifest = DirtyEvents::read(&layout::dirty_events_path(session_dir)).ok()?;
    if !manifest.is_current(size_cap) {
        return None;
    }
    session_key(session_dir, &manifest)
}

/// Compute the closure of the dirty inputs over `graph`, parse its files and
/// write them under `key`. An empty closure is stored too, so the next
/// refresh of the same dirty state reads 8 bytes and stops.
fn store(
    worktree_root: &Path,
    session_dir: &Path,
    graph: &ArchivedZeroCopyGraph,
    size_cap: SizeCap,
    key: u64,
) -> io::Result<()> {
    let dirty = overlay_reader::dirty_inputs_under(session_dir, worktree_root, size_cap)?;
    let files = overlay_reader::decode_view_inputs(&dirty.fragments);
    let closure = invalidation_closure(graph, &files, &dirty.tombstones);
    let entries = parse(worktree_root, &closure.paths, size_cap);
    write(
        session_dir,
        key,
        &ClosureFile {
            format: CLOSURE_FORMAT,
            builder_fingerprint: BUILDER_FINGERPRINT.to_string(),
            size_cap_bytes: size_cap.bytes(),
            stale: closure.stale,
            entries,
        },
    )
}

/// Parse each of `paths` as the dirty route parses a file. A path that
/// yields no parse (gone, over the cap, no provider) yields no entry: the
/// index holds nothing for it that a re-resolution could change.
fn parse(worktree_root: &Path, paths: &[String], size_cap: SizeCap) -> Vec<ClosureEntry> {
    if paths.is_empty() {
        return Vec::new();
    }
    // Stat before the parse, as the dirty route does: an edit landing
    // mid-parse records the old mtime, which the reader then refuses.
    let mtimes: HashMap<&str, u64> = paths
        .iter()
        .filter_map(|path| {
            let mtime = crate::auto_ensure::mtime_ns_of(&worktree_root.join(path))?;
            Some((path.as_str(), mtime))
        })
        .collect();
    let graphs = crate::reanalyze::reanalyze_expanded_under(
        worktree_root,
        &crate::git::DiffScope::Unstaged,
        paths,
        size_cap,
    );
    let mut entries: Vec<ClosureEntry> = graphs
        .iter()
        .filter_map(|graph| {
            let rel_path = graph.file_path.to_string_lossy().replace('\\', "/");
            let mtime_ns = *mtimes.get(rel_path.as_str())?;
            let fragment = super::overlay_writer::archive_fragments(graph).ok()?;
            Some(ClosureEntry {
                rel_path,
                mtime_ns,
                fragment,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::contracts::dirty_event::PathClass;
    use ecp_core::session::DirtyEventEntry;

    fn valid_fragment_bytes() -> Vec<u8> {
        let fragment = crate::session::overlay_writer::FragmentFile {
            symbols: vec![],
            imports: vec![],
            export_aliases: vec![],
            records: Default::default(),
            parsed_language: ecp_core::contracts::routing::ParsedLanguage::ABSENT,
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&fragment)
            .unwrap()
            .into_vec()
    }

    /// A session based at `base` under `dir/session`, a worktree under
    /// `dir/work` holding `files`, and one dirty input `dirty.ts`.
    fn session(dir: &Path, files: &[&str]) -> (PathBuf, PathBuf, DirtyEvents, ViewInputs) {
        let session_dir = dir.join("session");
        let worktree = dir.join("work");
        fs::create_dir_all(&session_dir).unwrap();
        fs::create_dir_all(worktree.join("src")).unwrap();
        for file in files {
            fs::write(worktree.join(file), "x").unwrap();
        }
        let meta = SessionMeta {
            version: 1,
            session_id: "s".into(),
            pid: None,
            started_at: "2026-10-26T00:00:00Z".into(),
            last_touched: "2026-10-26T00:00:00Z".into(),
            base_sha: "base".into(),
            source_worktree: worktree.display().to_string(),
            overlay_version: 0,
            watcher_pid: None,
            last_drained_offset: 0,
            agent_name: None,
        };
        SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &meta).unwrap();
        let manifest = manifest_with("src/dirty.ts", 1);
        let inputs = ViewInputs {
            fragments: vec![AcceptedFragment {
                rel_path: "src/dirty.ts".into(),
                bytes: valid_fragment_bytes(),
            }],
            tombstones: Vec::new(),
            stale: StaleClosure::default(),
            hash: 5,
        };
        (session_dir, worktree, manifest, inputs)
    }

    fn entry(worktree: &Path, path: &str, fragment: Vec<u8>) -> ClosureEntry {
        ClosureEntry {
            rel_path: path.into(),
            mtime_ns: crate::auto_ensure::mtime_ns_of(&worktree.join(path)).unwrap(),
            fragment,
        }
    }

    fn closure(stale: StaleClosure, entries: Vec<ClosureEntry>) -> ClosureFile {
        ClosureFile {
            format: CLOSURE_FORMAT,
            builder_fingerprint: BUILDER_FINGERPRINT.to_string(),
            size_cap_bytes: SizeCap::BUILT_IN.bytes(),
            stale,
            entries,
        }
    }

    /// Stands for: a dirty view whose manifest has no stored closure says
    /// so, and serves the dirty input alone.
    #[test]
    fn test_attach_no_closure_stored_marks_not_computed() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, mut inputs) = session(dir.path(), &[]);
        attach(
            &session_dir,
            &worktree,
            SizeCap::BUILT_IN,
            &manifest,
            &mut inputs,
        );
        assert!(inputs.stale.not_computed, "{:?}", inputs.stale);
        assert_eq!(inputs.fragments.len(), 1);
        assert_eq!(inputs.hash, 5, "nothing attached, the key is the dirty one");
    }

    /// Stands for: the stored closure of this manifest adds its current
    /// entries and its stale kinds, and changes the view key; a closure
    /// entry for a dirty path yields to the dirty parse.
    #[test]
    fn test_attach_stored_closure_serves_entries_and_stale() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, mut inputs) =
            session(dir.path(), &["src/caller.ts", "src/dirty.ts"]);
        let stale = StaleClosure {
            names: vec!["dup".into()],
            kinds: vec!["Calls".into()],
            ..StaleClosure::default()
        };
        let key = closure_key("base", &manifest);
        let file = closure(
            stale.clone(),
            vec![
                entry(&worktree, "src/caller.ts", valid_fragment_bytes()),
                entry(&worktree, "src/dirty.ts", valid_fragment_bytes()),
            ],
        );
        write(&session_dir, key, &file).unwrap();
        attach(
            &session_dir,
            &worktree,
            SizeCap::BUILT_IN,
            &manifest,
            &mut inputs,
        );
        let paths: Vec<&str> = inputs
            .fragments
            .iter()
            .map(|f| f.rel_path.as_str())
            .collect();
        assert_eq!(paths, vec!["src/dirty.ts", "src/caller.ts"]);
        assert_eq!(inputs.stale, stale);
        assert_ne!(inputs.hash, 5);
    }

    /// Stands for: a closure entry whose bin is corrupt, or whose file was
    /// touched since its parse, is not served, and the view says the
    /// closure is incomplete.
    #[test]
    fn test_attach_corrupt_or_touched_entry_marks_not_computed() {
        for corrupt in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let (session_dir, worktree, manifest, mut inputs) =
                session(dir.path(), &["src/caller.ts"]);
            let mut row = entry(&worktree, "src/caller.ts", valid_fragment_bytes());
            if corrupt {
                row.fragment = vec![0xff; 4];
            } else {
                row.mtime_ns += 1;
            }
            let key = closure_key("base", &manifest);
            write(
                &session_dir,
                key,
                &closure(StaleClosure::default(), vec![row]),
            )
            .unwrap();
            attach(
                &session_dir,
                &worktree,
                SizeCap::BUILT_IN,
                &manifest,
                &mut inputs,
            );
            assert_eq!(inputs.fragments.len(), 1, "corrupt={corrupt}");
            assert!(inputs.stale.not_computed, "corrupt={corrupt}");
        }
    }

    /// Stands for: a clean tree attaches nothing, whatever is stored.
    #[test]
    fn test_attach_empty_inputs_stays_empty() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, _) = session(dir.path(), &["src/caller.ts"]);
        let key = closure_key("base", &manifest);
        let row = entry(&worktree, "src/caller.ts", valid_fragment_bytes());
        write(
            &session_dir,
            key,
            &closure(StaleClosure::default(), vec![row]),
        )
        .unwrap();
        let mut inputs = ViewInputs {
            fragments: Vec::new(),
            tombstones: Vec::new(),
            stale: StaleClosure::default(),
            hash: 0,
        };
        attach(
            &session_dir,
            &worktree,
            SizeCap::BUILT_IN,
            &manifest,
            &mut inputs,
        );
        assert!(inputs.is_empty());
        assert!(inputs.stale.is_empty());
    }

    fn manifest_with(path: &str, mtime_ns: u64) -> DirtyEvents {
        let mut manifest = DirtyEvents::empty(SizeCap::BUILT_IN);
        let mut entry = DirtyEventEntry::tombstone(PathClass::SOURCE);
        entry.mtime_ns = mtime_ns;
        manifest.entries.insert(path.to_string(), entry);
        manifest
    }

    /// Stands for: a closure is keyed by every manifest row and the base
    /// SHA, so a closure is never served for another dirty state.
    #[test]
    fn test_closure_key_other_row_or_sha_differs() {
        let key = closure_key("a", &manifest_with("x.ts", 1));
        assert_eq!(key, closure_key("a", &manifest_with("x.ts", 1)));
        assert_ne!(key, closure_key("b", &manifest_with("x.ts", 1)));
        assert_ne!(key, closure_key("a", &manifest_with("x.ts", 2)));
        assert_ne!(key, closure_key("a", &manifest_with("y.ts", 1)));
        assert_ne!(
            key,
            closure_key("a", &DirtyEvents::empty(SizeCap::BUILT_IN))
        );
    }

    /// Stands for: a written closure reads back under its key only, and a
    /// truncated or absent file is no closure.
    #[test]
    fn test_closure_file_round_trip_other_key_or_truncated_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = ClosureFile {
            format: CLOSURE_FORMAT,
            builder_fingerprint: BUILDER_FINGERPRINT.to_string(),
            size_cap_bytes: SizeCap::BUILT_IN.bytes(),
            stale: StaleClosure {
                names: vec!["dup".into()],
                kinds: vec!["Calls".into()],
                ..StaleClosure::default()
            },
            entries: vec![ClosureEntry {
                rel_path: "src/b.js".into(),
                mtime_ns: 7,
                fragment: vec![1, 2, 3],
            }],
        };
        assert!(read(dir.path(), 9, SizeCap::BUILT_IN).is_none(), "absent");
        write(dir.path(), 9, &file).unwrap();
        assert_eq!(stored_key(dir.path()), Some(9));
        let back = read(dir.path(), 9, SizeCap::BUILT_IN).expect("same key");
        assert_eq!(back.stale, file.stale);
        assert_eq!(back.entries[0].rel_path, "src/b.js");
        assert_eq!(back.entries[0].fragment, vec![1, 2, 3]);
        assert!(
            read(dir.path(), 10, SizeCap::BUILT_IN).is_none(),
            "other key"
        );
        fs::write(closure_path(dir.path()), [9u8, 0, 0]).unwrap();
        assert!(
            read(dir.path(), 9, SizeCap::BUILT_IN).is_none(),
            "truncated"
        );
        assert_eq!(stored_key(dir.path()), None);
    }
}
