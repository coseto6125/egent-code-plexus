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
//! The dirty resolution configs travel here too (R-A4d): the refresh parses
//! each one once with the index's readers ([`config_edits`]) and stores the
//! result as [`ConfigEdits`] beside the closure it widens, so the closure
//! and the view read one parse under one key. The fragment manifest lists
//! source halves only, and no query parses a config.
//!
//! File layout: key (u64 LE) · rkyv [`ClosureFile`]. The key hashes what the
//! closure was computed from ([`closure_key`]): the session's base SHA,
//! every manifest row, and the path and mtime of each dirty config
//! ([`ConfigStamp`]). A reader serves the closure only under the key of the
//! manifest it reads and the stamps the file stores, and only while each
//! stamped config still has its stamp, so a closure never meets another
//! dirty state. A refresh that finds its key stored reads the file back and
//! keeps it only while every stored parse still has its file's mtime
//! ([`stored_is_current`]); the key itself covers no dependent.

use super::overlay_reader::{self, AcceptedFragment, ViewInputs};
use super::view_cache::hash_part;
use ecp_core::contracts::routing::SizeCap;
use ecp_core::contracts::workspace::{ConfigPresence, Ecosystem, ScopeAlias, WorkspaceFacts};
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::registry::BUILDER_FINGERPRINT;
use ecp_core::session::closure::{
    feeds_repo_wide, invalidation_closure_with_configs, ConfigEdits, StaleClosure,
};
use ecp_core::session::{layout, DirtyEvents, SessionMeta};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::Xxh3;

/// The closure file's name under `l1/`.
pub const CLOSURE_FILE: &str = "closure.bin";

/// Layout version of [`ClosureFile`]. A file of another version is no
/// closure: the next refresh rewrites it. Format 1 is unreleased, and R-A4d
/// extends it in place with the config stamps and edits.
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
    /// The dirty resolution configs the closure was computed over, in path
    /// order; part of the key.
    pub stamps: Vec<ConfigStamp>,
    /// Their parse, which the view merges over the archived config tables.
    pub configs: ConfigEdits,
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

/// One dirty resolution config as the refresh found it.
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct ConfigStamp {
    /// Repo-relative, `/`-separated.
    pub path: String,
    /// The mtime statted before the parse; `None` for a deleted config.
    pub mtime_ns: Option<u64>,
}

/// `<session>/l1/closure.bin`.
pub fn closure_path(session_dir: &Path) -> PathBuf {
    layout::l1_dir(session_dir).join(CLOSURE_FILE)
}

/// The key of a closure computed over `manifest` and the dirty configs
/// `stamps` for a session based at `base_sha`: every row, in manifest
/// order, then every stamp, in path order.
pub fn closure_key(base_sha: &str, manifest: &DirtyEvents, stamps: &[ConfigStamp]) -> u64 {
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
    for stamp in stamps {
        hash_part(&mut hasher, stamp.path.as_bytes());
        // An absent config and a config of mtime 0 hash apart.
        match stamp.mtime_ns {
            Some(mtime) => hash_part(&mut hasher, &mtime.to_le_bytes()),
            None => hash_part(&mut hasher, &[]),
        }
    }
    hasher.digest()
}

/// The session's base SHA; `None` without a readable session meta.
fn base_sha(session_dir: &Path) -> Option<String> {
    SessionMeta::read(&session_dir.join("session_meta.json"))
        .ok()
        .map(|meta| meta.base_sha)
}

/// The key the stored closure was written under; `None` without one.
fn stored_key(session_dir: &Path) -> Option<u64> {
    let mut head = [0u8; KEY_LEN];
    File::open(closure_path(session_dir))
        .and_then(|mut file| file.read_exact(&mut head))
        .ok()?;
    Some(u64::from_le_bytes(head))
}

/// The stored closure and the key it was written under, when this binary
/// wrote it under `size_cap`; `None` for another format, builder or cap, and
/// for bytes that fail validation. The caller checks the key against the
/// file's own stamps.
fn read(session_dir: &Path, size_cap: SizeCap) -> Option<(u64, ClosureFile)> {
    let bytes = fs::read(closure_path(session_dir)).ok()?;
    let (head, body) = bytes.split_at_checked(KEY_LEN)?;
    let key = u64::from_le_bytes(head.try_into().ok()?);
    // Checked access refuses misaligned input, and a `fs::read` buffer
    // carries no alignment guarantee past the 8-byte key.
    let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(body.len());
    aligned.extend_from_slice(body);
    let file = rkyv::from_bytes::<ClosureFile, rkyv::rancor::Error>(&aligned).ok()?;
    (file.format == CLOSURE_FORMAT
        && file.builder_fingerprint == BUILDER_FINGERPRINT
        && file.size_cap_bytes == size_cap.bytes())
    .then_some((key, file))
}

fn write(session_dir: &Path, key: u64, file: &ClosureFile) -> io::Result<()> {
    let body = rkyv::to_bytes::<rkyv::rancor::Error>(file).map_err(io::Error::other)?;
    let mut bytes = Vec::with_capacity(KEY_LEN + body.len());
    bytes.extend_from_slice(&key.to_le_bytes());
    bytes.extend_from_slice(&body);
    ecp_core::registry::atomic_write_bytes_no_fsync(&closure_path(session_dir), &bytes)
}

/// The stamp of each of `configs` (repo-relative), in path order.
fn stamps_of(worktree_root: &Path, configs: &[String]) -> Vec<ConfigStamp> {
    let paths: BTreeSet<&String> = configs.iter().collect();
    paths
        .into_iter()
        .map(|path| ConfigStamp {
            path: path.clone(),
            mtime_ns: crate::auto_ensure::mtime_ns_of(&worktree_root.join(path)),
        })
        .collect()
}

/// Add the closure stored for `manifest` (the current one under
/// `size_cap`) to `inputs`: each entry whose file still has its parsed mtime
/// and is no dirty path, hashed after the dirty inputs, then its stale
/// kinds, then the dirty configs' stamps and parse ([`ViewInputs::configs`]).
/// A dirty view with no closure stored for this manifest gets
/// `StaleClosure::not_computed`, and so does one whose closure file or dirty
/// config was touched since its parse: that file is served as indexed, and
/// a touched config serves no config edit. On a clean tree with no dirty
/// config the cost is an 8-byte read.
pub(crate) fn attach(
    session_dir: &Path,
    worktree_root: &Path,
    size_cap: SizeCap,
    manifest: &DirtyEvents,
    inputs: &mut ViewInputs,
) {
    let dirty = !inputs.is_empty();
    let not_computed = |inputs: &mut ViewInputs| {
        if dirty {
            inputs.stale = StaleClosure::not_computed();
        }
    };
    // A clean tree reads a closure only when one was stored over dirty
    // configs: no file, or the clean key, costs one open and 8 bytes.
    let stored = if dirty { None } else { stored_key(session_dir) };
    if !dirty && stored.is_none() {
        return;
    }
    let Some(base_sha) = base_sha(session_dir) else {
        return not_computed(inputs);
    };
    if stored == Some(closure_key(&base_sha, manifest, &[])) {
        return;
    }
    let Some(file) = read(session_dir, size_cap)
        .filter(|(key, file)| *key == closure_key(&base_sha, manifest, &file.stamps))
        .map(|(_, file)| file)
    else {
        return not_computed(inputs);
    };
    let configs_current = file.stamps.iter().all(|stamp| {
        crate::auto_ensure::mtime_ns_of(&worktree_root.join(&stamp.path)) == stamp.mtime_ns
    });
    if !configs_current {
        return not_computed(inputs);
    }
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
    // The stamps change with a config's mtime and the parse with its
    // bytes, so either edit moves the view key.
    hash_part(&mut hasher, format!("{:?}", file.stamps).as_bytes());
    hash_part(&mut hasher, format!("{:?}", file.configs).as_bytes());
    inputs.stale = stale;
    inputs.configs = file.configs;
    inputs.hash = hasher.digest();
}

/// Store the closure of the session's current manifest and the dirty
/// resolution configs `configs` (repo-relative, a deleted one included)
/// unless it is stored already: `auto_ensure` calls this after each L1
/// refresh and retire pass. The graph is loaded only when the stored key is
/// not the current one, or the stored file is not current
/// ([`stored_is_current`]). The closure also re-resolves the clean files the
/// configs rescope, and the file stores their parse for the view.
pub fn refresh_with_configs(
    worktree_root: &Path,
    session_dir: &Path,
    graph_path: &Path,
    size_cap: SizeCap,
    configs: &[String],
) -> io::Result<()> {
    let stamps = stamps_of(worktree_root, configs);
    let Some(key) = current_key(session_dir, size_cap, &stamps) else {
        return Ok(());
    };
    if stored_key(session_dir) == Some(key)
        && stored_is_current(worktree_root, session_dir, size_cap, key)
    {
        return Ok(());
    }
    let engine = crate::engine::Engine::load(graph_path)?;
    let graph = engine
        .graph()
        .map_err(|e| io::Error::other(e.to_string()))?;
    store(worktree_root, session_dir, graph, size_cap, key, stamps)
}

/// [`refresh_with_configs`] over a graph the caller holds, whatever key is
/// stored.
pub fn refresh_with_graph(
    worktree_root: &Path,
    session_dir: &Path,
    graph: &ArchivedZeroCopyGraph,
    size_cap: SizeCap,
    configs: &[String],
) -> io::Result<()> {
    let stamps = stamps_of(worktree_root, configs);
    match current_key(session_dir, size_cap, &stamps) {
        Some(key) => store(worktree_root, session_dir, graph, size_cap, key, stamps),
        None => Ok(()),
    }
}

/// The key of the current manifest and `stamps` under `size_cap`; `None`
/// without a current manifest or a session meta: the reader then serves no
/// dirty input either.
fn current_key(session_dir: &Path, size_cap: SizeCap, stamps: &[ConfigStamp]) -> Option<u64> {
    let manifest = DirtyEvents::read(&layout::dirty_events_path(session_dir)).ok()?;
    if !manifest.is_current(size_cap) {
        return None;
    }
    Some(closure_key(&base_sha(session_dir)?, &manifest, stamps))
}

/// Compute the closure of the dirty inputs and configs over `graph`, parse
/// its files and write them under `key`. An empty closure is stored too, so
/// the next refresh of the same dirty state reads 8 bytes and stops.
fn store(
    worktree_root: &Path,
    session_dir: &Path,
    graph: &ArchivedZeroCopyGraph,
    size_cap: SizeCap,
    key: u64,
    stamps: Vec<ConfigStamp>,
) -> io::Result<()> {
    let dirty = overlay_reader::dirty_inputs_under(session_dir, worktree_root, size_cap)?;
    let files = overlay_reader::decode_view_inputs(&dirty.fragments);
    let configs = config_edits(worktree_root, &graph.workspace_facts(), &stamps);
    let closure = invalidation_closure_with_configs(graph, &files, &dirty.tombstones, &configs);
    let (entries, omitted) = parse(worktree_root, &closure.paths, size_cap);
    let stale = if omitted {
        with_not_computed(closure.stale)
    } else {
        closure.stale
    };
    write(
        session_dir,
        key,
        &ClosureFile {
            format: CLOSURE_FORMAT,
            builder_fingerprint: BUILDER_FINGERPRINT.to_string(),
            size_cap_bytes: size_cap.bytes(),
            stale,
            entries,
            stamps,
            configs,
        },
    )
}

/// The parse of the dirty configs `stamps` over the archived scope table
/// `archived` (`ecp_core::session::closure`, "Resolution configs").
///
/// A config is kept (its archived rows stand) when the parser rejects it or
/// it is a `Cargo.toml`, whose rows the Rust module tree makes; then so is
/// every other dirty config of the same ecosystem, and, when a kept one
/// feeds the repo-wide entries, every one that does: a half-applied
/// ecosystem would pair new rows with old ones. Each other config is
/// applied: the rows at its directory and at each scope it invalidates are
/// re-derived from the archived configs of their ecosystems plus the
/// present dirty ones, and the repo-wide entries are re-read when it feeds
/// them. A merge that breaks a table invariant keeps every config.
///
/// Cost: one parse of each config of the touched ecosystems (the rows
/// cannot be re-derived from fewer, since a row reads its ancestors), plus
/// O(C · S) for C dirty configs over S archived rows. It runs once per
/// refresh, never per query.
pub(crate) fn config_edits(
    worktree_root: &Path,
    archived: &WorkspaceFacts,
    stamps: &[ConfigStamp],
) -> ConfigEdits {
    if stamps.is_empty() {
        return ConfigEdits::default();
    }
    let ecosystem_of = |path: &str| archived.config_ecosystem(path);
    let mut kept: BTreeSet<String> = BTreeSet::new();
    let mut applied: BTreeSet<String> = BTreeSet::new();
    for stamp in stamps {
        let path = &stamp.path;
        let rejected = ecosystem_of(path.as_str()) == Some(Ecosystem::Cargo)
            || crate::config_parser::config_rejected(worktree_root, path);
        let set = if rejected { &mut kept } else { &mut applied };
        set.insert(path.clone());
    }
    // Spread the kept set until it is closed under "same ecosystem" and
    // "both feed the repo-wide entries".
    loop {
        let kept_ecosystems: BTreeSet<Ecosystem> = kept
            .iter()
            .filter_map(|path| ecosystem_of(path.as_str()))
            .collect();
        let kept_repo_wide = kept.iter().any(|path| feeds_repo_wide(path.as_str()));
        let joins: Vec<String> = applied
            .iter()
            .filter(|path| {
                ecosystem_of(path.as_str()).is_some_and(|eco| kept_ecosystems.contains(&eco))
                    || (kept_repo_wide && feeds_repo_wide(path.as_str()))
            })
            .cloned()
            .collect();
        if joins.is_empty() {
            break;
        }
        for path in joins {
            applied.remove(&path);
            kept.insert(path);
        }
    }
    let keep_all = |applied: BTreeSet<String>, mut kept: BTreeSet<String>| {
        kept.extend(applied);
        ConfigEdits {
            kept: kept.into_iter().collect(),
            ..ConfigEdits::default()
        }
    };
    if applied.is_empty() {
        return keep_all(applied, kept);
    }

    let present = |path: &str| worktree_root.join(path).is_file();
    let mut roots: BTreeSet<(Ecosystem, String)> = BTreeSet::new();
    for path in &applied {
        if let Some((eco, _)) = Ecosystem::classify_config(file_name(path)) {
            if present(path.as_str()) {
                roots.insert((eco, parent_dir(path).to_string()));
            }
        }
        for id in archived.invalidated_by(path).scopes {
            if let Some(scope) = archived.scope(id) {
                roots.insert((scope.ecosystem, scope.root.clone()));
            }
        }
    }
    roots.retain(|(eco, _)| *eco != Ecosystem::Cargo);
    let ecosystems: BTreeSet<Ecosystem> = roots.iter().map(|(eco, _)| *eco).collect();
    let mut replaced: BTreeSet<String> = BTreeSet::new();
    let mut parse_paths: BTreeSet<String> = applied
        .iter()
        .filter(|path| present(path.as_str()))
        .cloned()
        .collect();
    for scope in &archived.scopes {
        if roots.contains(&(scope.ecosystem, scope.root.clone())) {
            replaced.insert(scope.manifest().to_string());
        }
        if ecosystems.contains(&scope.ecosystem) {
            parse_paths.extend(
                scope
                    .config_files
                    .iter()
                    .filter(|config| config.presence == ConfigPresence::Read)
                    .map(|config| config.path.clone())
                    .filter(|path| !applied.contains(path) || present(path.as_str())),
            );
        }
    }
    let parse_paths: Vec<String> = parse_paths.into_iter().collect();
    let rows: Vec<_> = crate::config_parser::parse_config_rows(worktree_root, &parse_paths)
        .into_iter()
        .filter(|draft| roots.contains(&(draft.scope.ecosystem, draft.scope.root.clone())))
        .collect();
    let repo_wide = applied
        .iter()
        .any(|path| feeds_repo_wide(path.as_str()))
        .then(|| repo_wide_entries(worktree_root, archived, &applied));
    let replaced: Vec<String> = replaced.into_iter().collect();
    if archived.with_rows(&replaced, &rows).is_err() {
        return keep_all(applied, kept);
    }
    ConfigEdits {
        applied: applied.into_iter().collect(),
        kept: kept.into_iter().collect(),
        replaced,
        rows,
        repo_wide,
    }
}

/// The repo-wide alias entries after the dirty configs `applied`: the root
/// configs on disk, then the `ProjectReference`s of the archived .NET
/// projects with each dirty project added or removed, in path order.
fn repo_wide_entries(
    worktree_root: &Path,
    archived: &WorkspaceFacts,
    applied: &BTreeSet<String>,
) -> Vec<ScopeAlias> {
    let mut projects: BTreeSet<String> = archived
        .scopes
        .iter()
        .filter(|scope| scope.ecosystem == Ecosystem::DotNet)
        .map(|scope| scope.manifest().to_string())
        .collect();
    for path in applied {
        if !matches!(
            Ecosystem::classify_config(file_name(path)),
            Some((Ecosystem::DotNet, _))
        ) {
            continue;
        }
        if worktree_root.join(path).is_file() {
            projects.insert(path.clone());
        } else {
            projects.remove(path);
        }
    }
    let projects: Vec<String> = projects.into_iter().collect();
    crate::config_parser::repo_wide_aliases(worktree_root, &projects)
        .entries()
        .map(|(pattern, replacements)| ScopeAlias {
            pattern: pattern.to_string(),
            replacements: replacements.to_vec(),
        })
        .collect()
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Parse each of `paths` as the dirty route parses a file, and report
/// whether any parse was omitted. A path that yields no graph (gone, no
/// provider) yields no entry: the index holds nothing for it that a
/// re-resolution could change. A path whose parse was omitted (unreadable,
/// over the cap) yields no entry either, but the index holds its edges and
/// the closure did not re-resolve them: the caller records that. Stored as a
/// fragment, an omitted parse would read as an empty file and mask every
/// indexed declaration and edge of it.
fn parse(worktree_root: &Path, paths: &[String], size_cap: SizeCap) -> (Vec<ClosureEntry>, bool) {
    if paths.is_empty() {
        return (Vec::new(), false);
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
    let omitted = graphs.iter().any(is_omitted);
    let mut entries: Vec<ClosureEntry> = graphs
        .iter()
        .filter(|graph| !is_omitted(graph))
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
    (entries, omitted)
}

/// `LocalGraph::omitted` leaves `content_hash` zero on purpose and records
/// the reason as a blind spot; a parsed file hashes its bytes.
fn is_omitted(graph: &ecp_core::analyzer::types::LocalGraph) -> bool {
    graph.content_hash == [0u8; 8] && !graph.blind_spots.is_empty()
}

/// `stale` with the closure marked incomplete: a clean file whose re-parse
/// was omitted keeps its indexed edges of every rebuilt kind, which is what
/// `StaleClosure::not_computed` says. The names and configs the closure
/// found stay.
fn with_not_computed(mut stale: StaleClosure) -> StaleClosure {
    let all = StaleClosure::not_computed();
    stale.not_computed = true;
    stale.kinds.extend(all.kinds);
    stale.kinds.sort_unstable();
    stale.kinds.dedup();
    stale
}

/// Whether the stored file already is the closure to serve under `key`: it
/// reads back whole, every stored parse still has its file's mtime and a
/// valid fragment, and it is not marked incomplete (a retry may now read the
/// file that failed). The refresh key covers the manifest and the config
/// stamps but no dependent's mtime, so this is what notices a touched
/// dependent. Cost: one read and at most `CLOSURE_LIMIT` stats.
fn stored_is_current(
    worktree_root: &Path,
    session_dir: &Path,
    size_cap: SizeCap,
    key: u64,
) -> bool {
    read(session_dir, size_cap).is_some_and(|(stored, file)| {
        stored == key
            && !file.stale.not_computed
            && file.entries.iter().all(|entry| {
                crate::auto_ensure::mtime_ns_of(&worktree_root.join(&entry.rel_path))
                    == Some(entry.mtime_ns)
                    && overlay_reader::valid_fragment(&entry.fragment)
            })
    })
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
            configs: ConfigEdits::default(),
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
            stamps: Vec::new(),
            configs: ConfigEdits::default(),
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
        let key = closure_key("base", &manifest, &[]);
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
            let key = closure_key("base", &manifest, &[]);
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
        let key = closure_key("base", &manifest, &[]);
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
            configs: ConfigEdits::default(),
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
        let key = closure_key("a", &manifest_with("x.ts", 1), &[]);
        assert_eq!(key, closure_key("a", &manifest_with("x.ts", 1), &[]));
        assert_ne!(key, closure_key("b", &manifest_with("x.ts", 1), &[]));
        assert_ne!(key, closure_key("a", &manifest_with("x.ts", 2), &[]));
        assert_ne!(key, closure_key("a", &manifest_with("y.ts", 1), &[]));
        assert_ne!(
            key,
            closure_key("a", &DirtyEvents::empty(SizeCap::BUILT_IN), &[])
        );
    }

    fn stamp(path: &str, mtime_ns: Option<u64>) -> ConfigStamp {
        ConfigStamp {
            path: path.into(),
            mtime_ns,
        }
    }

    /// Stands for: a dirty config's edit, deletion or appearance moves the
    /// key, so a closure computed over other config bytes is never served.
    #[test]
    fn test_closure_key_other_config_stamp_differs() {
        let manifest = manifest_with("x.ts", 1);
        let key = closure_key("a", &manifest, &[stamp("tsconfig.json", Some(1))]);
        assert_eq!(
            key,
            closure_key("a", &manifest, &[stamp("tsconfig.json", Some(1))])
        );
        for other in [
            vec![],
            vec![stamp("tsconfig.json", Some(2))],
            vec![stamp("tsconfig.json", None)],
            vec![stamp("tsconfig.json", Some(0))],
            vec![stamp("go.mod", Some(1))],
            vec![stamp("go.mod", Some(1)), stamp("tsconfig.json", Some(1))],
        ] {
            assert_ne!(key, closure_key("a", &manifest, &other), "{other:?}");
        }
        assert_ne!(
            closure_key("a", &manifest, &[stamp("tsconfig.json", None)]),
            closure_key("a", &manifest, &[stamp("tsconfig.json", Some(0))]),
            "absent and mtime 0 hash apart"
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
                configs: vec!["bad/tsconfig.json".into()],
                ..StaleClosure::default()
            },
            entries: vec![ClosureEntry {
                rel_path: "src/b.js".into(),
                mtime_ns: 7,
                fragment: vec![1, 2, 3],
            }],
            stamps: vec![stamp("go.mod", None)],
            configs: ConfigEdits {
                applied: vec!["go.mod".into()],
                replaced: vec!["go.mod".into()],
                ..ConfigEdits::default()
            },
        };
        assert!(read(dir.path(), SizeCap::BUILT_IN).is_none(), "absent");
        write(dir.path(), 9, &file).unwrap();
        assert_eq!(stored_key(dir.path()), Some(9));
        let (key, back) = read(dir.path(), SizeCap::BUILT_IN).expect("written");
        assert_eq!(key, 9);
        assert_eq!(back.stale, file.stale);
        assert_eq!(back.entries[0].rel_path, "src/b.js");
        assert_eq!(back.entries[0].fragment, vec![1, 2, 3]);
        assert_eq!(back.stamps, file.stamps);
        assert_eq!(back.configs, file.configs);
        assert!(
            read(dir.path(), SizeCap::resolve(Some("1"), None)).is_none(),
            "other cap"
        );
        fs::write(closure_path(dir.path()), [9u8, 0, 0]).unwrap();
        assert!(read(dir.path(), SizeCap::BUILT_IN).is_none(), "truncated");
        assert_eq!(stored_key(dir.path()), None);
    }

    /// A closure over the dirty config `tsconfig.json` (present in
    /// `worktree`), its stamp current, and one applied edit.
    fn config_closure(worktree: &Path) -> ClosureFile {
        fs::write(worktree.join("tsconfig.json"), "{}").unwrap();
        let mut file = closure(StaleClosure::default(), Vec::new());
        file.stamps = stamps_of(worktree, &["tsconfig.json".to_string()]);
        file.configs = ConfigEdits {
            applied: vec!["tsconfig.json".into()],
            repo_wide: Some(Vec::new()),
            ..ConfigEdits::default()
        };
        file
    }

    /// Stands for: the stored config parse reaches the view inputs and
    /// moves the view key, beside the dirty files.
    #[test]
    fn test_attach_config_closure_serves_configs_and_moves_hash() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, mut inputs) = session(dir.path(), &[]);
        let file = config_closure(&worktree);
        let key = closure_key("base", &manifest, &file.stamps);
        let configs = file.configs.clone();
        write(&session_dir, key, &file).unwrap();
        attach(
            &session_dir,
            &worktree,
            SizeCap::BUILT_IN,
            &manifest,
            &mut inputs,
        );
        assert_eq!(inputs.configs, configs);
        assert!(inputs.stale.is_empty(), "{:?}", inputs.stale);
        assert_ne!(inputs.hash, 5);
    }

    /// Stands for: a dirty state of configs alone (no dirty source) is no
    /// clean tree: the view inputs carry the configs.
    #[test]
    fn test_attach_config_only_dirty_state_serves_configs() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, _, _) = session(dir.path(), &[]);
        let manifest = DirtyEvents::empty(SizeCap::BUILT_IN);
        let file = config_closure(&worktree);
        let key = closure_key("base", &manifest, &file.stamps);
        write(&session_dir, key, &file).unwrap();
        let mut inputs = ViewInputs {
            fragments: Vec::new(),
            tombstones: Vec::new(),
            stale: StaleClosure::default(),
            configs: ConfigEdits::default(),
            hash: 0,
        };
        attach(
            &session_dir,
            &worktree,
            SizeCap::BUILT_IN,
            &manifest,
            &mut inputs,
        );
        assert!(!inputs.is_empty());
        assert_eq!(inputs.configs.applied, vec!["tsconfig.json".to_string()]);
        assert_ne!(inputs.hash, 0);
    }

    /// Stands for: a config touched since the refresh parsed it serves no
    /// config edit, and a dirty view says the closure is not computed.
    #[test]
    fn test_attach_config_touched_since_parse_marks_not_computed() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, mut inputs) = session(dir.path(), &[]);
        let mut file = config_closure(&worktree);
        file.stamps[0].mtime_ns = file.stamps[0].mtime_ns.map(|m| m + 1);
        let key = closure_key("base", &manifest, &file.stamps);
        write(&session_dir, key, &file).unwrap();
        attach(
            &session_dir,
            &worktree,
            SizeCap::BUILT_IN,
            &manifest,
            &mut inputs,
        );
        assert!(inputs.configs.is_empty());
        assert!(inputs.stale.not_computed);
        assert_eq!(inputs.hash, 5);
    }

    /// A tree of `files` and the scope rows the index stores for it.
    fn indexed(files: &[(&str, &str)]) -> (tempfile::TempDir, WorkspaceFacts) {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        let paths: Vec<String> = files.iter().map(|(path, _)| path.to_string()).collect();
        let facts = crate::config_parser::parse_repo_configs(dir.path(), &paths).workspace;
        (dir, facts)
    }

    fn rewrite(root: &Path, path: &str, text: &str) {
        fs::write(root.join(path), text).unwrap();
    }

    fn config_stamps(root: &Path, paths: &[&str]) -> Vec<ConfigStamp> {
        let paths: Vec<String> = paths.iter().map(|path| path.to_string()).collect();
        stamps_of(root, &paths)
    }

    const NODE_PAIR: [(&str, &str); 4] = [
        (
            "packages/a/package.json",
            r#"{"name":"a","dependencies":{"b":"*"}}"#,
        ),
        ("packages/b/package.json", r#"{"name":"b"}"#),
        ("packages/a/src/x.ts", "export const x = 1;"),
        ("packages/b/src/y.ts", "export const y = 1;"),
    ];

    /// Stands for: no dirty config parses nothing.
    #[test]
    fn test_config_edits_no_stamp_returns_empty() {
        let (dir, facts) = indexed(&NODE_PAIR);
        assert!(config_edits(dir.path(), &facts, &[]).is_empty());
    }

    /// Stands for: an edited config replaces the rows at its root with the
    /// index's reading of the new bytes, and the merge yields the table a
    /// fresh index of the edited tree stores.
    #[test]
    fn test_config_edits_renamed_package_rows_equal_fresh_index() {
        let (dir, facts) = indexed(&NODE_PAIR);
        rewrite(dir.path(), "packages/b/package.json", r#"{"name":"c"}"#);
        let edits = config_edits(
            dir.path(),
            &facts,
            &config_stamps(dir.path(), &["packages/b/package.json"]),
        );
        assert_eq!(edits.applied, vec!["packages/b/package.json".to_string()]);
        assert!(edits.kept.is_empty());
        assert!(edits
            .replaced
            .contains(&"packages/b/package.json".to_string()));
        assert_eq!(
            edits.repo_wide, None,
            "a nested config feeds no repo-wide entry"
        );
        let merged = facts.with_rows(&edits.replaced, &edits.rows).unwrap();
        let (_, fresh) = indexed(&[
            NODE_PAIR[0],
            ("packages/b/package.json", r#"{"name":"c"}"#),
            NODE_PAIR[2],
            NODE_PAIR[3],
        ]);
        assert_eq!(merged, fresh);
    }

    /// Stands for: a deleted config removes its rows and adds none.
    #[test]
    fn test_config_edits_deleted_config_removes_rows() {
        let (dir, facts) = indexed(&NODE_PAIR);
        fs::remove_file(dir.path().join("packages/b/package.json")).unwrap();
        let edits = config_edits(
            dir.path(),
            &facts,
            &config_stamps(dir.path(), &["packages/b/package.json"]),
        );
        assert_eq!(edits.applied, vec!["packages/b/package.json".to_string()]);
        let merged = facts.with_rows(&edits.replaced, &edits.rows).unwrap();
        assert!(merged
            .scopes
            .iter()
            .all(|scope| scope.manifest() != "packages/b/package.json"));
    }

    /// Stands for: a config the reader rejects (malformed or whitespace-only
    /// bytes) keeps its archived rows, and so does every other dirty config
    /// of its ecosystem.
    #[test]
    fn test_config_edits_malformed_config_kept_with_its_ecosystem() {
        for bad in ["{ not json", "   "] {
            let (dir, facts) = indexed(&NODE_PAIR);
            rewrite(dir.path(), "packages/a/package.json", bad);
            rewrite(dir.path(), "packages/b/package.json", r#"{"name":"c"}"#);
            let edits = config_edits(
                dir.path(),
                &facts,
                &config_stamps(
                    dir.path(),
                    &["packages/a/package.json", "packages/b/package.json"],
                ),
            );
            assert!(edits.applied.is_empty(), "{bad:?}: {edits:?}");
            assert_eq!(
                edits.kept,
                vec![
                    "packages/a/package.json".to_string(),
                    "packages/b/package.json".to_string()
                ],
                "{bad:?}"
            );
            assert!(edits.replaced.is_empty() && edits.rows.is_empty());
            assert!(!edits.is_empty());
        }
    }

    /// Stands for: well-formed bytes of the wrong shape (a list where the
    /// reader expects an object) are no rejection: the edit applies, and the
    /// merge equals the fresh index's table.
    #[test]
    fn test_config_edits_list_for_object_applies_equal_fresh_index() {
        let (dir, facts) = indexed(&NODE_PAIR);
        rewrite(dir.path(), "packages/b/package.json", "[]");
        let edits = config_edits(
            dir.path(),
            &facts,
            &config_stamps(dir.path(), &["packages/b/package.json"]),
        );
        assert_eq!(edits.applied, vec!["packages/b/package.json".to_string()]);
        let merged = facts.with_rows(&edits.replaced, &edits.rows).unwrap();
        let (_, fresh) = indexed(&[
            NODE_PAIR[0],
            ("packages/b/package.json", "[]"),
            NODE_PAIR[2],
            NODE_PAIR[3],
        ]);
        assert_eq!(merged, fresh);
    }

    /// Stands for: a `Cargo.toml` is kept (the Rust module tree makes its
    /// rows), and a config of another ecosystem still applies.
    #[test]
    fn test_config_edits_cargo_toml_kept_other_ecosystem_applied() {
        let (dir, facts) = indexed(&NODE_PAIR);
        fs::create_dir_all(dir.path().join("crates/k")).unwrap();
        rewrite(
            dir.path(),
            "crates/k/Cargo.toml",
            "[package]\nname = \"k\"\n",
        );
        rewrite(dir.path(), "packages/b/package.json", r#"{"name":"c"}"#);
        let edits = config_edits(
            dir.path(),
            &facts,
            &config_stamps(
                dir.path(),
                &["crates/k/Cargo.toml", "packages/b/package.json"],
            ),
        );
        assert_eq!(edits.kept, vec!["crates/k/Cargo.toml".to_string()]);
        assert_eq!(edits.applied, vec!["packages/b/package.json".to_string()]);
    }

    /// Stands for: a root config that feeds the repo-wide entries re-reads
    /// them, equal to the index's reading of the new bytes.
    #[test]
    fn test_config_edits_root_tsconfig_rereads_repo_wide_entries() {
        let tsconfig = r#"{"compilerOptions":{"paths":{"@/*":["src/a/*"]}}}"#;
        let (dir, facts) = indexed(&[("tsconfig.json", tsconfig), ("src/a/u.ts", "")]);
        let moved = r#"{"compilerOptions":{"paths":{"@/*":["src/b/*"]}}}"#;
        rewrite(dir.path(), "tsconfig.json", moved);
        let edits = config_edits(
            dir.path(),
            &facts,
            &config_stamps(dir.path(), &["tsconfig.json"]),
        );
        let entries = edits.repo_wide.expect("re-read");
        let fresh = crate::config_parser::parse_configs(dir.path());
        let fresh: Vec<(String, Vec<String>)> = fresh
            .entries()
            .map(|(pattern, replacements)| (pattern.to_string(), replacements.to_vec()))
            .collect();
        let got: Vec<(String, Vec<String>)> = entries
            .into_iter()
            .map(|alias| (alias.pattern, alias.replacements))
            .collect();
        assert_eq!(got, fresh);
        assert!(got.iter().any(|(_, r)| r == &vec!["src/b/*".to_string()]));
    }

    fn current_key_of(manifest: &DirtyEvents) -> u64 {
        closure_key("base", manifest, &[])
    }

    /// Stands for: a stored closure whose dependents all keep their mtime
    /// and fragments is kept, so a refresh of the same dirty state loads no
    /// graph.
    #[test]
    fn test_stored_is_current_untouched_closure_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, _) = session(dir.path(), &["src/caller.ts"]);
        let key = current_key_of(&manifest);
        let row = entry(&worktree, "src/caller.ts", valid_fragment_bytes());
        write(
            &session_dir,
            key,
            &closure(StaleClosure::default(), vec![row]),
        )
        .unwrap();
        assert!(stored_is_current(
            &worktree,
            &session_dir,
            SizeCap::BUILT_IN,
            key
        ));
        // A second identical call leaves the same answer and one valid file.
        assert!(stored_is_current(
            &worktree,
            &session_dir,
            SizeCap::BUILT_IN,
            key
        ));
        assert!(read(&session_dir, SizeCap::BUILT_IN).is_some());
    }

    /// Stands for: an empty dependent list is a current closure, and a
    /// stored key other than the asked one is not.
    #[test]
    fn test_stored_is_current_empty_closure_current_other_key_not() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, _) = session(dir.path(), &[]);
        let key = current_key_of(&manifest);
        write(&session_dir, key, &closure(StaleClosure::default(), vec![])).unwrap();
        assert!(stored_is_current(
            &worktree,
            &session_dir,
            SizeCap::BUILT_IN,
            key
        ));
        assert!(!stored_is_current(
            &worktree,
            &session_dir,
            SizeCap::BUILT_IN,
            key + 1
        ));
    }

    /// Stands for: an editor saving unchanged bytes moves a dependent's
    /// mtime; `attach` refuses that entry, so the refresh must recompute
    /// instead of keeping a closure the reader no longer serves.
    #[test]
    fn test_refresh_after_dependent_touched_recomputes_closure() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, mut inputs) = session(dir.path(), &["src/caller.ts"]);
        let key = current_key_of(&manifest);
        let row = entry(&worktree, "src/caller.ts", valid_fragment_bytes());
        write(
            &session_dir,
            key,
            &closure(StaleClosure::default(), vec![row]),
        )
        .unwrap();
        let path = worktree.join("src/caller.ts");
        let before = crate::auto_ensure::mtime_ns_of(&path).unwrap();
        filetime::set_file_mtime(
            &path,
            filetime::FileTime::from_unix_time((before / 1_000_000_000) as i64 + 10, 0),
        )
        .unwrap();
        assert_ne!(crate::auto_ensure::mtime_ns_of(&path), Some(before));
        assert!(!stored_is_current(
            &worktree,
            &session_dir,
            SizeCap::BUILT_IN,
            key
        ));
        // The reader agrees with the predicate: it refuses the touched entry.
        attach(
            &session_dir,
            &worktree,
            SizeCap::BUILT_IN,
            &manifest,
            &mut inputs,
        );
        assert!(inputs.stale.not_computed);
    }

    /// Stands for: a dependent removed from disk since the parse is no
    /// current closure.
    #[test]
    fn test_stored_is_current_dependent_removed_is_not_current() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, _) = session(dir.path(), &["src/caller.ts"]);
        let key = current_key_of(&manifest);
        let row = entry(&worktree, "src/caller.ts", valid_fragment_bytes());
        write(
            &session_dir,
            key,
            &closure(StaleClosure::default(), vec![row]),
        )
        .unwrap();
        fs::remove_file(worktree.join("src/caller.ts")).unwrap();
        assert!(!stored_is_current(
            &worktree,
            &session_dir,
            SizeCap::BUILT_IN,
            key
        ));
    }

    /// Stands for: a stored file whose body fails validation, or whose
    /// fragment is malformed, is recomputed rather than kept.
    #[test]
    fn test_refresh_corrupt_body_recomputes() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, _) = session(dir.path(), &["src/caller.ts"]);
        let key = current_key_of(&manifest);
        let row = entry(&worktree, "src/caller.ts", valid_fragment_bytes());
        write(
            &session_dir,
            key,
            &closure(StaleClosure::default(), vec![row]),
        )
        .unwrap();
        let path = closure_path(&session_dir);
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..KEY_LEN + 3]).unwrap();
        assert_eq!(stored_key(&session_dir), Some(key), "the key still reads");
        assert!(!stored_is_current(
            &worktree,
            &session_dir,
            SizeCap::BUILT_IN,
            key
        ));

        let bad = entry(&worktree, "src/caller.ts", vec![0xff; 4]);
        write(
            &session_dir,
            key,
            &closure(StaleClosure::default(), vec![bad]),
        )
        .unwrap();
        assert!(!stored_is_current(
            &worktree,
            &session_dir,
            SizeCap::BUILT_IN,
            key
        ));
    }

    /// Stands for: a closure stored as incomplete (a dependent's parse was
    /// omitted) is retried by the next refresh, since the file may be
    /// readable now and the key does not move when it is.
    #[test]
    fn test_stored_is_current_not_computed_closure_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (session_dir, worktree, manifest, _) = session(dir.path(), &[]);
        let key = current_key_of(&manifest);
        write(
            &session_dir,
            key,
            &closure(with_not_computed(StaleClosure::default()), vec![]),
        )
        .unwrap();
        assert!(!stored_is_current(
            &worktree,
            &session_dir,
            SizeCap::BUILT_IN,
            key
        ));
    }

    /// Stands for: a dependent that cannot be read at refresh time is not
    /// stored as an empty fragment (which would mask its indexed edges),
    /// and the parse reports the omission so the stored stale closure names
    /// the rebuilt kinds.
    #[test]
    fn test_refresh_dependent_parse_omitted_is_not_stored_as_empty() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (_, worktree, _, _) = session(dir.path(), &[]);
        fs::write(worktree.join("src/ok.ts"), "export const a = 1;\n").unwrap();
        fs::write(worktree.join("src/locked.ts"), "export const b = 2;\n").unwrap();
        let locked = worktree.join("src/locked.ts");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&locked).is_ok() {
            return; // root reads past the mode: the failure cannot be planted
        }
        let paths = vec!["src/locked.ts".to_string(), "src/ok.ts".to_string()];
        let (entries, omitted) = parse(&worktree, &paths, SizeCap::BUILT_IN);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        let stored: Vec<&str> = entries.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(stored, vec!["src/ok.ts"]);
        assert!(omitted);
        let stale = with_not_computed(StaleClosure::default());
        assert!(stale.not_computed && !stale.is_empty());
    }

    /// Stands for: an empty dependent list parses to nothing and omits nothing.
    #[test]
    fn test_parse_empty_paths_yields_nothing_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let (entries, omitted) = parse(dir.path(), &[], SizeCap::BUILT_IN);
        assert!(entries.is_empty() && !omitted);
    }

    /// Stands for: the names and configs the closure found survive the
    /// not-computed marking, and the kinds are the union.
    #[test]
    fn test_with_not_computed_keeps_names_configs_and_kinds() {
        let stale = StaleClosure {
            names: vec!["dup".into()],
            configs: vec!["tsconfig.json".into()],
            kinds: vec!["Zzz".into()],
            ..StaleClosure::default()
        };
        let marked = with_not_computed(stale);
        assert!(marked.not_computed);
        assert_eq!(marked.names, vec!["dup".to_string()]);
        assert_eq!(marked.configs, vec!["tsconfig.json".to_string()]);
        assert!(marked.kinds.contains(&"Zzz".to_string()));
        assert!(marked.kinds.len() > 1);
        assert!(marked.kinds.windows(2).all(|w| w[0] < w[1]));
    }
}
