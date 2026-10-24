//! Contract C3: dirty event (`docs/specs/contracts/C3-dirty-event.md`).
//!
//! One record per changed path of a worktree against its base commit:
//! added, modified, deleted, renamed with both halves, or unmerged (existence
//! unknown until the reader stats the file). Each path carries
//! a class: source (contract C5 routes it), resolution config (contract C2
//! names it), both, or neither ("other"). Only an event whose every path is
//! "other" may skip the per-path syscalls of the freshness gate.
//!
//! Paths are relative to the worktree top and follow the path rule of C2:
//! `/`-separated, no leading `./`, no trailing `/`, no `.` or `..` segment,
//! no `\`. Everything here is pure: nothing reads the file system or runs
//! git, so a deleted path is classified like a present one. No production
//! code calls these types yet.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::contracts::routing::{FileRouter, RouteOutcome};
use crate::contracts::workspace::{Ecosystem, WorkspaceFacts};

/// A path relative to the worktree top, in the C2 path rule. Never empty.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoPath(String);

/// A path that breaks the C2 path rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotRepoRelative(pub String);

impl RepoPath {
    pub fn new(path: impl Into<String>) -> Result<Self, NotRepoRelative> {
        let path = path.into();
        if super::workspace::is_normal_path(&path) {
            Ok(Self(path))
        } else {
            Err(NotRepoRelative(path))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The last segment.
    pub fn file_name(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map_or(self.0.as_str(), |(_, name)| name)
    }
}

impl AsRef<RepoPath> for RepoPath {
    fn as_ref(&self) -> &RepoPath {
        self
    }
}

/// What a changed path is to the graph: a set of two flags. Empty is
/// "other". Decided from the path alone, so it needs no syscall.
///
/// Stored in the dirty manifest by the C3 behaviour slice; the JSON field
/// names `source` and `resolution_config` are the stored form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct PathClass {
    source: bool,
    resolution_config: bool,
}

impl PathClass {
    pub const OTHER: Self = Self::new(false, false);
    pub const SOURCE: Self = Self::new(true, false);
    pub const RESOLUTION_CONFIG: Self = Self::new(false, true);

    pub const fn new(source: bool, resolution_config: bool) -> Self {
        Self {
            source,
            resolution_config,
        }
    }

    /// Source when C5 routes the path (`RouteOutcome::Routed`). Resolution
    /// config when C2 names it: by the table when `facts` is given
    /// (`WorkspaceFacts::config_ecosystem`), else by the file name alone
    /// (`Ecosystem::classify_config`). The name-only form misses a config
    /// file that only a scope's `config_files` lists, such as an `extends`
    /// target.
    pub fn classify(path: &RepoPath, router: &FileRouter, facts: Option<&WorkspaceFacts>) -> Self {
        let source = matches!(router.route(path.as_str()), RouteOutcome::Routed(_));
        let resolution_config = match facts {
            Some(facts) => facts.config_ecosystem(path.as_str()).is_some(),
            None => Ecosystem::classify_config(path.file_name()).is_some(),
        };
        Self::new(source, resolution_config)
    }

    pub const fn is_source(self) -> bool {
        self.source
    }

    pub const fn is_resolution_config(self) -> bool {
        self.resolution_config
    }

    /// Neither source nor resolution config: the only class that may skip
    /// the per-path syscalls.
    pub const fn is_other(self) -> bool {
        !self.source && !self.resolution_config
    }

    pub const fn union(self, other: Self) -> Self {
        Self::new(
            self.source || other.source,
            self.resolution_config || other.resolution_config,
        )
    }
}

/// The state of one path against the base commit, stored as `added`,
/// `modified`, `deleted`. `unmerged` exists only before the reader's stat:
/// a writer stores the confirmed change ([`DirtyEvent::confirm`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathChange {
    Added,
    Modified,
    Deleted,
    Unmerged,
}

/// One record of the dirty set. `P` is [`RepoPath`] before classification
/// and [`ClassifiedPath`] after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirtyEvent<P = RepoPath> {
    /// The worktree holds the path, and the base commit does not.
    Added(P),
    /// The worktree holds the path. The base commit holds it too.
    Modified(P),
    /// The worktree lacks the path. The base commit holds it. A reader
    /// drops every base node and edge of the file: a whole-file tombstone,
    /// never "keep base".
    Deleted(P),
    /// `from` as in `Deleted`, `to` as in `Added`, and git paired them.
    /// The two paths differ.
    Renamed { from: P, to: P },
    /// An unmerged entry (`U` in a column, `AA`, `DD`). Its status letters
    /// describe the index stages, not the file: a `UU` file can be deleted
    /// by hand, a `DD` path recreated. The base commit and the worktree are
    /// both unknown. The reader stats the path once and calls
    /// [`DirtyEvent::confirm`] before it reads any half.
    Unmerged(P),
}

impl<P> DirtyEvent<P> {
    /// The half whose current content a reader parses: `Added`, `Modified`,
    /// the `to` of `Renamed`.
    pub fn worktree_path(&self) -> Option<&P> {
        match self {
            Self::Added(path) | Self::Modified(path) => Some(path),
            Self::Renamed { to, .. } => Some(to),
            Self::Deleted(_) | Self::Unmerged(_) => None,
        }
    }

    /// The half a reader tombstones: `Deleted`, the `from` of `Renamed`.
    pub fn removed_path(&self) -> Option<&P> {
        match self {
            Self::Deleted(path) => Some(path),
            Self::Renamed { from, .. } => Some(from),
            Self::Added(_) | Self::Modified(_) | Self::Unmerged(_) => None,
        }
    }

    /// The event after the reader's stat of an `Unmerged` path: `Modified`
    /// when the file exists, else `Deleted`. Every other variant is
    /// returned unchanged, so `exists` is read only for `Unmerged`.
    pub fn confirm(self, exists: bool) -> Self {
        match self {
            Self::Unmerged(path) if exists => Self::Modified(path),
            Self::Unmerged(path) => Self::Deleted(path),
            other => other,
        }
    }

    /// Each path with its own change: a rename yields `(Deleted, from)`,
    /// then `(Added, to)`. This is the per-path form the manifest stores.
    pub fn halves(&self) -> impl Iterator<Item = (PathChange, &P)> + '_ {
        let (first, second) = match self {
            Self::Added(path) => ((PathChange::Added, path), None),
            Self::Modified(path) => ((PathChange::Modified, path), None),
            Self::Deleted(path) => ((PathChange::Deleted, path), None),
            Self::Unmerged(path) => ((PathChange::Unmerged, path), None),
            Self::Renamed { from, to } => {
                ((PathChange::Deleted, from), Some((PathChange::Added, to)))
            }
        };
        std::iter::once(first).chain(second)
    }

    pub fn map<Q>(self, mut f: impl FnMut(P) -> Q) -> DirtyEvent<Q> {
        match self {
            Self::Added(path) => DirtyEvent::Added(f(path)),
            Self::Modified(path) => DirtyEvent::Modified(f(path)),
            Self::Deleted(path) => DirtyEvent::Deleted(f(path)),
            Self::Unmerged(path) => DirtyEvent::Unmerged(f(path)),
            Self::Renamed { from, to } => DirtyEvent::Renamed {
                from: f(from),
                to: f(to),
            },
        }
    }

    fn first(&self) -> &P {
        match self {
            Self::Added(path)
            | Self::Modified(path)
            | Self::Deleted(path)
            | Self::Unmerged(path) => path,
            Self::Renamed { from, .. } => from,
        }
    }
}

/// A path with its class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedPath {
    pub path: RepoPath,
    pub class: PathClass,
}

impl AsRef<RepoPath> for ClassifiedPath {
    fn as_ref(&self) -> &RepoPath {
        &self.path
    }
}

impl DirtyEvent<ClassifiedPath> {
    /// The union of the classes of every half.
    pub fn class(&self) -> PathClass {
        self.halves()
            .fold(PathClass::OTHER, |class, (_, path)| class.union(path.class))
    }

    /// Every half is "other": the event may skip every per-path syscall.
    pub fn is_other(&self) -> bool {
        self.class().is_other()
    }
}

/// A porcelain entry that names a change C3 cannot represent. A reader
/// reports it in its caveat; it never guesses the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub path_lossy: String,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    NotUtf8,
    NotRepoRelative,
}

/// Two events name one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicatePath(pub RepoPath);

/// Output that is not `git status --porcelain -z` (v1). The gate then has
/// no dirty set and takes its inconclusive branch, as for a failed status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PorcelainError {
    /// An entry is shorter than `XY p`, or byte 2 is not a space.
    Malformed { offset: usize },
    /// The last entry has no terminating NUL.
    Unterminated { offset: usize },
    /// A status pair outside porcelain v1.
    UnknownStatus { offset: usize, code: [u8; 2] },
    /// A rename or copy entry without its source segment.
    MissingSource { offset: usize },
}

/// The dirty set of one worktree. Each path appears in one event and in
/// one half only; events are sorted by their first half's path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtySet<P = RepoPath> {
    events: Vec<DirtyEvent<P>>,
    skipped: Vec<SkippedEntry>,
}

fn key<P: AsRef<RepoPath>>(path: &P) -> &RepoPath {
    path.as_ref()
}

impl<P: AsRef<RepoPath>> DirtySet<P> {
    /// A set from events another producer made (the mtime walk gives
    /// `Modified` only). Rejects a path named twice.
    pub fn from_events(events: Vec<DirtyEvent<P>>) -> Result<Self, DuplicatePath> {
        {
            let mut seen: BTreeSet<&RepoPath> = BTreeSet::new();
            for event in &events {
                for (_, path) in event.halves() {
                    if !seen.insert(key(path)) {
                        return Err(DuplicatePath(key(path).clone()));
                    }
                }
            }
        }
        Ok(Self::sorted(events, Vec::new()))
    }

    fn sorted(mut events: Vec<DirtyEvent<P>>, skipped: Vec<SkippedEntry>) -> Self {
        events.sort_by(|a, b| key(a.first()).cmp(key(b.first())));
        Self { events, skipped }
    }

    pub fn events(&self) -> &[DirtyEvent<P>] {
        &self.events
    }

    pub fn skipped(&self) -> &[SkippedEntry] {
        &self.skipped
    }

    /// No event and no skipped entry.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.skipped.is_empty()
    }
}

impl DirtySet<RepoPath> {
    /// Parse the stdout of `git status --porcelain -z --untracked-files=all`
    /// (format v1). Entries are read in order; a rename or copy entry is
    /// followed by its source segment. Several entries for one path are
    /// merged into one record (`D  a` with `?? a` is `Modified`).
    pub fn from_porcelain_z(stdout: &[u8]) -> Result<Self, PorcelainError> {
        let mut parse = PorcelainParse::default();
        let mut pos = 0;
        while let Some((offset, entry)) = next_segment(stdout, &mut pos)? {
            if entry.len() < 4 || entry[2] != b' ' {
                return Err(PorcelainError::Malformed { offset });
            }
            let (x, y, path) = (entry[0], entry[1], &entry[3..]);
            if !STATUS_CODES.contains(&x) || !STATUS_CODES.contains(&y) {
                return Err(PorcelainError::UnknownStatus {
                    offset,
                    code: [x, y],
                });
            }
            match (x, y) {
                (b'?', b'?') => {
                    parse.note(path, Head::Lacks, Worktree::Present);
                }
                (b'!', b'!') => {}
                _ if matches!(x, b'?' | b'!') || matches!(y, b'?' | b'!') => {
                    return Err(PorcelainError::UnknownStatus {
                        offset,
                        code: [x, y],
                    });
                }
                _ if is_unmerged(x, y) => {
                    parse.note(path, Head::Unknown, Worktree::Unknown);
                }
                _ if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') => {
                    let Some((_, source)) = next_segment(stdout, &mut pos)? else {
                        return Err(PorcelainError::MissingSource { offset });
                    };
                    let to = parse.note(path, Head::Lacks, Worktree::unless(y == b'D'));
                    // A copy leaves its source unchanged.
                    if x == b'R' || y == b'R' {
                        let from = parse.note(source, Head::Has, Worktree::Absent);
                        if let (Some(from), Some(to)) = (from, to) {
                            parse.renames.push((from, to));
                        }
                    }
                }
                _ => {
                    let head = if x == b'A' { Head::Lacks } else { Head::Has };
                    parse.note(path, head, Worktree::unless(x == b'D' || y == b'D'));
                }
            }
        }
        Ok(parse.finish())
    }

    /// Classify every path. Order and skipped entries are kept.
    pub fn classify(
        self,
        router: &FileRouter,
        facts: Option<&WorkspaceFacts>,
    ) -> DirtySet<ClassifiedPath> {
        let events = self
            .events
            .into_iter()
            .map(|event| {
                event.map(|path| {
                    let class = PathClass::classify(&path, router, facts);
                    ClassifiedPath { path, class }
                })
            })
            .collect();
        DirtySet {
            events,
            skipped: self.skipped,
        }
    }
}

impl DirtySet<ClassifiedPath> {
    /// Drop every event whose halves are all "other". The freshness gate
    /// calls this before any per-path syscall. A rename with one "other"
    /// half stays; the reader checks each half's class.
    pub fn without_other(mut self) -> Self {
        self.events.retain(|event| !event.is_other());
        self
    }
}

/// Every status letter of porcelain v1 (`git status` documentation, "Short
/// Format").
const STATUS_CODES: &[u8] = b" MTADRCU?!";

fn is_unmerged(x: u8, y: u8) -> bool {
    x == b'U' || y == b'U' || (x, y) == (b'A', b'A') || (x, y) == (b'D', b'D')
}

fn next_segment<'a>(
    stdout: &'a [u8],
    pos: &mut usize,
) -> Result<Option<(usize, &'a [u8])>, PorcelainError> {
    let start = *pos;
    if start >= stdout.len() {
        return Ok(None);
    }
    let len = stdout[start..]
        .iter()
        .position(|&b| b == 0)
        .ok_or(PorcelainError::Unterminated { offset: start })?;
    *pos = start + len + 1;
    Ok(Some((start, &stdout[start..start + len])))
}

/// What the entries say about the base commit. Merging takes the maximum:
/// a staged deletion (`Has`) beats the untracked entry of the same path
/// (`Lacks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
enum Head {
    #[default]
    Unknown,
    Lacks,
    Has,
}

/// What the entries say about the worktree file. Merging takes the
/// maximum: an entry that proves presence (`?? a`) beats one that implies
/// absence (`D  a`), which beats an unmerged entry that says nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
enum Worktree {
    #[default]
    Unknown,
    Absent,
    Present,
}

impl Worktree {
    fn unless(absent: bool) -> Self {
        if absent {
            Self::Absent
        } else {
            Self::Present
        }
    }
}

#[derive(Debug, Default)]
struct PathFacts {
    head: Head,
    worktree: Worktree,
}

#[derive(Debug, Default)]
struct PorcelainParse {
    facts: BTreeMap<RepoPath, PathFacts>,
    renames: Vec<(RepoPath, RepoPath)>,
    skipped: Vec<SkippedEntry>,
}

impl PorcelainParse {
    fn note(&mut self, raw: &[u8], head: Head, worktree: Worktree) -> Option<RepoPath> {
        let Ok(text) = std::str::from_utf8(raw) else {
            self.skipped.push(SkippedEntry {
                path_lossy: String::from_utf8_lossy(raw).into_owned(),
                reason: SkipReason::NotUtf8,
            });
            return None;
        };
        let Ok(path) = RepoPath::new(text) else {
            self.skipped.push(SkippedEntry {
                path_lossy: text.to_owned(),
                reason: SkipReason::NotRepoRelative,
            });
            return None;
        };
        let facts = self.facts.entry(path.clone()).or_default();
        facts.head = facts.head.max(head);
        facts.worktree = facts.worktree.max(worktree);
        Some(path)
    }

    fn finish(self) -> DirtySet {
        let mut changes: BTreeMap<RepoPath, PathChange> = self
            .facts
            .into_iter()
            .filter_map(|(path, facts)| {
                let change = match (facts.worktree, facts.head) {
                    (Worktree::Unknown, _) => PathChange::Unmerged,
                    (Worktree::Present, Head::Lacks) => PathChange::Added,
                    (Worktree::Present, _) => PathChange::Modified,
                    (Worktree::Absent, Head::Lacks) => return None,
                    (Worktree::Absent, _) => PathChange::Deleted,
                };
                Some((path, change))
            })
            .collect();
        let mut events = Vec::with_capacity(changes.len());
        for (from, to) in self.renames {
            if changes.get(&from) == Some(&PathChange::Deleted)
                && changes.get(&to) == Some(&PathChange::Added)
            {
                changes.remove(&from);
                changes.remove(&to);
                events.push(DirtyEvent::Renamed { from, to });
            }
        }
        events.extend(changes.into_iter().map(|(path, change)| match change {
            PathChange::Added => DirtyEvent::Added(path),
            PathChange::Modified => DirtyEvent::Modified(path),
            PathChange::Deleted => DirtyEvent::Deleted(path),
            PathChange::Unmerged => DirtyEvent::Unmerged(path),
        }));
        DirtySet::sorted(events, self.skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::routing::SizeCap;
    use crate::contracts::workspace::{ConfigPresence, ConfigRead, Membership, Scope};

    fn path(p: &str) -> RepoPath {
        RepoPath::new(p).unwrap()
    }

    fn parse(stdout: &[u8]) -> Vec<DirtyEvent> {
        DirtySet::from_porcelain_z(stdout)
            .unwrap()
            .events()
            .to_vec()
    }

    fn router() -> FileRouter {
        FileRouter::new(SizeCap::BUILT_IN)
    }

    fn class_of(p: &str, facts: Option<&WorkspaceFacts>) -> PathClass {
        PathClass::classify(&path(p), &router(), facts)
    }

    /// One Node scope at `web` whose tsconfig extends a file with an
    /// unrouted extension and no config name, and tried one that is missing.
    fn facts_with_extends_target() -> WorkspaceFacts {
        let read = |path: &str, presence| ConfigRead {
            path: path.to_string(),
            presence,
        };
        let scope = Scope {
            ecosystem: Ecosystem::Node,
            root: "web".to_string(),
            config_files: vec![
                read("web/tsconfig.json", ConfigPresence::Read),
                read("build/tsconfig.shared.jsonc", ConfigPresence::Read),
                read("build/tsconfig.later.jsonc", ConfigPresence::Missing),
            ],
            membership: Membership::Subtree,
            entry_files: Vec::new(),
            aliases: Vec::new(),
            search_roots: Vec::new(),
            dependencies: Vec::new(),
        };
        WorkspaceFacts::new(vec![scope], Vec::new()).unwrap()
    }

    fn classified(p: &str, class: PathClass) -> ClassifiedPath {
        ClassifiedPath {
            path: path(p),
            class,
        }
    }

    // ── RepoPath ────────────────────────────────────────────────────────

    #[test]
    fn test_repo_path_new_nested_path_accepts() {
        assert_eq!(path("src/a b/c.py").as_str(), "src/a b/c.py");
    }

    #[test]
    fn test_repo_path_new_irregular_forms_rejects() {
        for bad in [
            "", "./a.py", "a/", "a//b", "a/../b", "a/./b", "/abs", "a\\b.py",
        ] {
            assert_eq!(
                RepoPath::new(bad),
                Err(NotRepoRelative(bad.to_string())),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn test_repo_path_file_name_nested_and_top_level_returns_last_segment() {
        assert_eq!(path("a/b/Cargo.toml").file_name(), "Cargo.toml");
        assert_eq!(path("go.mod").file_name(), "go.mod");
    }

    // ── PathClass ───────────────────────────────────────────────────────

    #[test]
    fn test_path_class_constants_flags_match_names() {
        assert!(PathClass::OTHER.is_other());
        assert!(PathClass::SOURCE.is_source() && !PathClass::SOURCE.is_resolution_config());
        assert!(PathClass::RESOLUTION_CONFIG.is_resolution_config());
        assert!(!PathClass::RESOLUTION_CONFIG.is_source());
        assert_eq!(PathClass::default(), PathClass::OTHER);
    }

    #[test]
    fn test_path_class_union_source_and_config_holds_both() {
        let both = PathClass::SOURCE.union(PathClass::RESOLUTION_CONFIG);
        assert_eq!(both, PathClass::new(true, true));
        assert!(!both.is_other());
        assert_eq!(PathClass::OTHER.union(PathClass::OTHER), PathClass::OTHER);
    }

    #[test]
    fn test_classify_python_file_is_source_only() {
        assert_eq!(class_of("pkg/a.py", None), PathClass::SOURCE);
    }

    #[test]
    fn test_classify_csv_file_is_other() {
        assert_eq!(class_of("data/rows.csv", None), PathClass::OTHER);
    }

    #[test]
    fn test_classify_config_names_without_route_are_config_only() {
        for p in [
            "Cargo.toml",
            "crates/x/Cargo.toml",
            "go.mod",
            "go.work",
            "pyproject.toml",
        ] {
            assert_eq!(class_of(p, None), PathClass::RESOLUTION_CONFIG, "{p}");
        }
        assert_eq!(
            class_of("src/App.csproj", None),
            PathClass::RESOLUTION_CONFIG
        );
    }

    #[test]
    fn test_classify_tsconfig_json_is_source_and_config() {
        // `.json` routes to the OpenAPI content gate (C5), and the name is a
        // C2 config: C5 open point 12, the class is a set.
        assert_eq!(
            class_of("packages/a/tsconfig.json", None),
            PathClass::new(true, true)
        );
        assert_eq!(class_of("package.json", None), PathClass::new(true, true));
    }

    #[test]
    fn test_classify_jest_config_ts_is_source_and_config() {
        assert_eq!(class_of("jest.config.ts", None), PathClass::new(true, true));
    }

    #[test]
    fn test_classify_template_dir_path_is_other() {
        // C5 excludes it; an excluded path is not source.
        assert_eq!(class_of("__brick__/lib/a.py", None), PathClass::OTHER);
    }

    #[test]
    fn test_classify_extends_target_without_facts_is_other() {
        assert_eq!(
            class_of("build/tsconfig.shared.jsonc", None),
            PathClass::OTHER
        );
    }

    #[test]
    fn test_classify_extends_target_with_facts_is_config() {
        let facts = facts_with_extends_target();
        assert_eq!(
            class_of("build/tsconfig.shared.jsonc", Some(&facts)),
            PathClass::RESOLUTION_CONFIG
        );
        // The table keeps the name rule too.
        assert_eq!(
            class_of("other/go.mod", Some(&facts)),
            PathClass::RESOLUTION_CONFIG
        );
    }

    #[test]
    fn test_classify_missing_extends_target_with_facts_is_config() {
        // Its appearance changes the effective config (C2 `ConfigPresence`).
        let facts = facts_with_extends_target();
        assert_eq!(
            class_of("build/tsconfig.later.jsonc", Some(&facts)),
            PathClass::RESOLUTION_CONFIG
        );
    }

    #[test]
    fn test_path_class_serde_field_names_round_trip() {
        let json = serde_json::to_string(&PathClass::new(true, false)).unwrap();
        assert_eq!(json, r#"{"source":true,"resolution_config":false}"#);
        let back: PathClass = serde_json::from_str(&json).unwrap();
        assert_eq!(back, PathClass::SOURCE);
    }

    #[test]
    fn test_path_change_serde_snake_case_round_trips() {
        for (change, text) in [
            (PathChange::Added, "\"added\""),
            (PathChange::Modified, "\"modified\""),
            (PathChange::Deleted, "\"deleted\""),
            (PathChange::Unmerged, "\"unmerged\""),
        ] {
            assert_eq!(serde_json::to_string(&change).unwrap(), text);
            assert_eq!(serde_json::from_str::<PathChange>(text).unwrap(), change);
        }
    }

    // ── DirtyEvent ──────────────────────────────────────────────────────

    #[test]
    fn test_dirty_event_worktree_path_each_variant_returns_present_half() {
        assert_eq!(
            DirtyEvent::Added(path("a")).worktree_path(),
            Some(&path("a"))
        );
        assert_eq!(
            DirtyEvent::Modified(path("a")).worktree_path(),
            Some(&path("a"))
        );
        assert_eq!(DirtyEvent::Deleted(path("a")).worktree_path(), None);
        let renamed = DirtyEvent::Renamed {
            from: path("a"),
            to: path("b"),
        };
        assert_eq!(renamed.worktree_path(), Some(&path("b")));
    }

    #[test]
    fn test_dirty_event_removed_path_each_variant_returns_gone_half() {
        assert_eq!(DirtyEvent::Added(path("a")).removed_path(), None);
        assert_eq!(DirtyEvent::Modified(path("a")).removed_path(), None);
        assert_eq!(
            DirtyEvent::Deleted(path("a")).removed_path(),
            Some(&path("a"))
        );
        let renamed = DirtyEvent::Renamed {
            from: path("a"),
            to: path("b"),
        };
        assert_eq!(renamed.removed_path(), Some(&path("a")));
    }

    #[test]
    fn test_dirty_event_halves_renamed_yields_deleted_then_added() {
        let renamed = DirtyEvent::Renamed {
            from: path("a"),
            to: path("b"),
        };
        let halves: Vec<_> = renamed.halves().collect();
        assert_eq!(
            halves,
            vec![
                (PathChange::Deleted, &path("a")),
                (PathChange::Added, &path("b"))
            ]
        );
        let modified = DirtyEvent::Modified(path("c"));
        assert_eq!(
            modified.halves().collect::<Vec<_>>(),
            vec![(PathChange::Modified, &path("c"))]
        );
    }

    #[test]
    fn test_dirty_event_unmerged_has_no_half_to_read_until_confirmed() {
        let unmerged = DirtyEvent::Unmerged(path("c.py"));
        assert_eq!(unmerged.worktree_path(), None);
        assert_eq!(unmerged.removed_path(), None);
        assert_eq!(
            unmerged.halves().collect::<Vec<_>>(),
            vec![(PathChange::Unmerged, &path("c.py"))]
        );
    }

    #[test]
    fn test_dirty_event_confirm_unmerged_file_present_returns_modified() {
        assert_eq!(
            DirtyEvent::Unmerged(path("c.py")).confirm(true),
            DirtyEvent::Modified(path("c.py"))
        );
    }

    #[test]
    fn test_dirty_event_confirm_unmerged_file_deleted_by_hand_returns_deleted() {
        assert_eq!(
            DirtyEvent::Unmerged(path("c.py")).confirm(false),
            DirtyEvent::Deleted(path("c.py"))
        );
    }

    #[test]
    fn test_dirty_event_confirm_other_variants_ignores_stat() {
        let renamed = DirtyEvent::Renamed {
            from: path("a"),
            to: path("b"),
        };
        for event in [
            DirtyEvent::Added(path("a")),
            DirtyEvent::Modified(path("a")),
            DirtyEvent::Deleted(path("a")),
            renamed,
        ] {
            assert_eq!(event.clone().confirm(true), event);
            assert_eq!(event.clone().confirm(false), event);
        }
    }

    #[test]
    fn test_dirty_event_map_renamed_maps_both_halves() {
        let renamed = DirtyEvent::Renamed {
            from: path("a"),
            to: path("b"),
        };
        let lengths = renamed.map(|p| p.as_str().len());
        assert_eq!(lengths, DirtyEvent::Renamed { from: 1, to: 1 });
    }

    #[test]
    fn test_dirty_event_class_renamed_unions_halves() {
        let renamed = DirtyEvent::Renamed {
            from: classified("a.py", PathClass::SOURCE),
            to: classified("a.py.bak", PathClass::OTHER),
        };
        assert_eq!(renamed.class(), PathClass::SOURCE);
        assert!(!renamed.is_other());
        let both_other = DirtyEvent::Renamed {
            from: classified("a.csv", PathClass::OTHER),
            to: classified("b.csv", PathClass::OTHER),
        };
        assert!(both_other.is_other());
    }

    // ── from_porcelain_z ────────────────────────────────────────────────

    #[test]
    fn test_from_porcelain_z_empty_output_returns_empty_set() {
        let set = DirtySet::from_porcelain_z(b"").unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn test_from_porcelain_z_worktree_modification_returns_modified() {
        assert_eq!(
            parse(b" M a.py\0"),
            vec![DirtyEvent::Modified(path("a.py"))]
        );
        assert_eq!(
            parse(b"MM a.py\0"),
            vec![DirtyEvent::Modified(path("a.py"))]
        );
        assert_eq!(
            parse(b" T link\0"),
            vec![DirtyEvent::Modified(path("link"))]
        );
    }

    #[test]
    fn test_from_porcelain_z_untracked_returns_added() {
        assert_eq!(
            parse(b"?? new.py\0"),
            vec![DirtyEvent::Added(path("new.py"))]
        );
    }

    #[test]
    fn test_from_porcelain_z_staged_addition_returns_added() {
        assert_eq!(
            parse(b"A  new.py\0"),
            vec![DirtyEvent::Added(path("new.py"))]
        );
        assert_eq!(
            parse(b"AM new.py\0"),
            vec![DirtyEvent::Added(path("new.py"))]
        );
    }

    #[test]
    fn test_from_porcelain_z_worktree_deletion_returns_deleted() {
        assert_eq!(
            parse(b" D gone.py\0"),
            vec![DirtyEvent::Deleted(path("gone.py"))]
        );
        assert_eq!(
            parse(b"MD gone.py\0"),
            vec![DirtyEvent::Deleted(path("gone.py"))]
        );
    }

    #[test]
    fn test_from_porcelain_z_staged_deletion_returns_deleted() {
        assert_eq!(
            parse(b"D  gone.py\0"),
            vec![DirtyEvent::Deleted(path("gone.py"))]
        );
    }

    #[test]
    fn test_from_porcelain_z_untracked_after_staged_deletion_returns_modified() {
        // `git rm --cached a.py`: HEAD holds it, the worktree still holds it.
        assert_eq!(
            parse(b"D  a.py\0?? a.py\0"),
            vec![DirtyEvent::Modified(path("a.py"))]
        );
        assert_eq!(
            parse(b"?? a.py\0D  a.py\0"),
            vec![DirtyEvent::Modified(path("a.py"))]
        );
    }

    #[test]
    fn test_from_porcelain_z_added_then_deleted_returns_no_record() {
        assert!(DirtySet::from_porcelain_z(b"AD tmp.py\0")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_from_porcelain_z_staged_rename_returns_renamed() {
        assert_eq!(
            parse(b"R  src/new.ts\0src/old.ts\0"),
            vec![DirtyEvent::Renamed {
                from: path("src/old.ts"),
                to: path("src/new.ts"),
            }]
        );
    }

    #[test]
    fn test_from_porcelain_z_rename_modified_returns_renamed() {
        assert_eq!(
            parse(b"RM new.py\0old.py\0"),
            vec![DirtyEvent::Renamed {
                from: path("old.py"),
                to: path("new.py"),
            }]
        );
    }

    #[test]
    fn test_from_porcelain_z_worktree_rename_returns_renamed() {
        // An intent-to-add destination: git pairs it in the worktree column.
        assert_eq!(
            parse(b" R new.py\0old.py\0"),
            vec![DirtyEvent::Renamed {
                from: path("old.py"),
                to: path("new.py"),
            }]
        );
    }

    #[test]
    fn test_from_porcelain_z_rename_destination_deleted_returns_deleted_source() {
        assert_eq!(
            parse(b"RD new.py\0old.py\0"),
            vec![DirtyEvent::Deleted(path("old.py"))]
        );
    }

    #[test]
    fn test_from_porcelain_z_rename_source_recreated_returns_added_and_modified() {
        assert_eq!(
            parse(b"R  b.py\0a.py\0?? a.py\0"),
            vec![
                DirtyEvent::Modified(path("a.py")),
                DirtyEvent::Added(path("b.py")),
            ]
        );
    }

    #[test]
    fn test_from_porcelain_z_copy_returns_added_destination_only() {
        assert_eq!(
            parse(b"C  copy.py\0orig.py\0"),
            vec![DirtyEvent::Added(path("copy.py"))]
        );
    }

    #[test]
    fn test_from_porcelain_z_rename_source_with_space_at_third_byte_keeps_source() {
        // Today's `parse_porcelain_paths` reads any segment with a space at
        // byte 2 as an entry, so this source became the path `c.py`.
        assert_eq!(
            parse(b"R  new.py\0ab c.py\0"),
            vec![DirtyEvent::Renamed {
                from: path("ab c.py"),
                to: path("new.py"),
            }]
        );
    }

    #[test]
    fn test_from_porcelain_z_unmerged_codes_return_unmerged() {
        // The letters describe index stages, never the file on disk.
        for code in [*b"UU", *b"AA", *b"AU", *b"UA", *b"DU", *b"UD", *b"DD"] {
            let mut stdout = code.to_vec();
            stdout.extend_from_slice(b" c.py\0");
            assert_eq!(
                parse(&stdout),
                vec![DirtyEvent::Unmerged(path("c.py"))],
                "{}",
                String::from_utf8_lossy(&code)
            );
        }
    }

    #[test]
    fn test_from_porcelain_z_unmerged_beside_other_entries_keeps_only_its_path_unknown() {
        assert_eq!(
            parse(b"UU c.py\0 M a.py\0"),
            vec![
                DirtyEvent::Modified(path("a.py")),
                DirtyEvent::Unmerged(path("c.py")),
            ]
        );
    }

    #[test]
    fn test_from_porcelain_z_ignored_entry_returns_no_record() {
        assert!(DirtySet::from_porcelain_z(b"!! target/x\0")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_from_porcelain_z_spaces_and_non_ascii_in_path_keeps_path() {
        assert_eq!(
            parse(" M src/a b/文件.ts\0".as_bytes()),
            vec![DirtyEvent::Modified(path("src/a b/文件.ts"))]
        );
    }

    #[test]
    fn test_from_porcelain_z_mixed_entries_sorted_by_first_path() {
        assert_eq!(
            parse(b"?? z.py\0 M a.py\0R  m.py\0b.py\0"),
            vec![
                DirtyEvent::Modified(path("a.py")),
                DirtyEvent::Renamed {
                    from: path("b.py"),
                    to: path("m.py"),
                },
                DirtyEvent::Added(path("z.py")),
            ]
        );
    }

    #[test]
    fn test_from_porcelain_z_non_utf8_path_records_skipped() {
        let set = DirtySet::from_porcelain_z(b" M bad\xff.py\0 M ok.py\0").unwrap();
        assert_eq!(set.events(), &[DirtyEvent::Modified(path("ok.py"))]);
        assert_eq!(set.skipped().len(), 1);
        assert_eq!(set.skipped()[0].reason, SkipReason::NotUtf8);
    }

    #[test]
    fn test_from_porcelain_z_backslash_path_records_skipped() {
        let set = DirtySet::from_porcelain_z(b"?? a\\b.py\0").unwrap();
        assert!(set.events().is_empty());
        assert_eq!(
            set.skipped(),
            &[SkippedEntry {
                path_lossy: "a\\b.py".to_string(),
                reason: SkipReason::NotRepoRelative,
            }]
        );
        assert!(!set.is_empty());
    }

    #[test]
    fn test_from_porcelain_z_rename_with_skipped_destination_returns_deleted_source() {
        let set = DirtySet::from_porcelain_z(b"R  new\xff.py\0old.py\0").unwrap();
        assert_eq!(set.events(), &[DirtyEvent::Deleted(path("old.py"))]);
        assert_eq!(set.skipped().len(), 1);
    }

    #[test]
    fn test_from_porcelain_z_short_entry_returns_malformed() {
        assert_eq!(
            DirtySet::from_porcelain_z(b" M a.py\0M\0"),
            Err(PorcelainError::Malformed { offset: 8 })
        );
    }

    #[test]
    fn test_from_porcelain_z_missing_separator_returns_malformed() {
        assert_eq!(
            DirtySet::from_porcelain_z(b"MMxa.py\0"),
            Err(PorcelainError::Malformed { offset: 0 })
        );
    }

    #[test]
    fn test_from_porcelain_z_unterminated_entry_returns_unterminated() {
        assert_eq!(
            DirtySet::from_porcelain_z(b" M a.py"),
            Err(PorcelainError::Unterminated { offset: 0 })
        );
    }

    #[test]
    fn test_from_porcelain_z_rename_without_source_returns_missing_source() {
        assert_eq!(
            DirtySet::from_porcelain_z(b"R  new.py\0"),
            Err(PorcelainError::MissingSource { offset: 0 })
        );
    }

    #[test]
    fn test_from_porcelain_z_unknown_status_returns_unknown_status() {
        assert_eq!(
            DirtySet::from_porcelain_z(b"XY a.py\0"),
            Err(PorcelainError::UnknownStatus {
                offset: 0,
                code: *b"XY",
            })
        );
        assert_eq!(
            DirtySet::from_porcelain_z(b"?M a.py\0"),
            Err(PorcelainError::UnknownStatus {
                offset: 0,
                code: *b"?M",
            })
        );
    }

    // ── DirtySet ────────────────────────────────────────────────────────

    #[test]
    fn test_from_events_duplicate_path_returns_error() {
        let events = vec![
            DirtyEvent::Modified(path("a.py")),
            DirtyEvent::Deleted(path("a.py")),
        ];
        assert_eq!(
            DirtySet::from_events(events),
            Err(DuplicatePath(path("a.py")))
        );
    }

    #[test]
    fn test_from_events_rename_half_reused_returns_error() {
        let events = vec![
            DirtyEvent::Renamed {
                from: path("a.py"),
                to: path("b.py"),
            },
            DirtyEvent::Added(path("b.py")),
        ];
        assert_eq!(
            DirtySet::from_events(events),
            Err(DuplicatePath(path("b.py")))
        );
    }

    #[test]
    fn test_from_events_rename_onto_itself_returns_error() {
        let events = vec![DirtyEvent::Renamed {
            from: path("a.py"),
            to: path("a.py"),
        }];
        assert_eq!(
            DirtySet::from_events(events),
            Err(DuplicatePath(path("a.py")))
        );
    }

    #[test]
    fn test_from_events_unsorted_input_returns_sorted() {
        let set = DirtySet::from_events(vec![
            DirtyEvent::Modified(path("z.rs")),
            DirtyEvent::Modified(path("a.rs")),
        ])
        .unwrap();
        assert_eq!(
            set.events(),
            &[
                DirtyEvent::Modified(path("a.rs")),
                DirtyEvent::Modified(path("z.rs")),
            ]
        );
        assert!(set.skipped().is_empty());
    }

    #[test]
    fn test_classify_dirty_set_keeps_order_and_skipped() {
        let set = DirtySet::from_porcelain_z(b"?? b.csv\0 M a.py\0?? c\\d\0")
            .unwrap()
            .classify(&router(), None);
        assert_eq!(
            set.events(),
            &[
                DirtyEvent::Modified(classified("a.py", PathClass::SOURCE)),
                DirtyEvent::Added(classified("b.csv", PathClass::OTHER)),
            ]
        );
        assert_eq!(set.skipped().len(), 1);
    }

    #[test]
    fn test_without_other_csv_py_and_cargo_toml_keeps_py_and_cargo_toml() {
        let set = DirtySet::from_porcelain_z(b"?? a.csv\0?? b.csv\0 M lib.py\0 M Cargo.toml\0")
            .unwrap()
            .classify(&router(), None)
            .without_other();
        assert_eq!(
            set.events(),
            &[
                DirtyEvent::Modified(classified("Cargo.toml", PathClass::RESOLUTION_CONFIG)),
                DirtyEvent::Modified(classified("lib.py", PathClass::SOURCE)),
            ]
        );
    }

    #[test]
    fn test_without_other_deleted_source_keeps_tombstone() {
        let set = DirtySet::from_porcelain_z(b" D gone.py\0 D gone.csv\0")
            .unwrap()
            .classify(&router(), None)
            .without_other();
        assert_eq!(
            set.events(),
            &[DirtyEvent::Deleted(classified(
                "gone.py",
                PathClass::SOURCE
            ))]
        );
    }

    #[test]
    fn test_without_other_rename_to_other_extension_keeps_event() {
        let set = DirtySet::from_porcelain_z(b"R  a.py.bak\0a.py\0")
            .unwrap()
            .classify(&router(), None)
            .without_other();
        assert_eq!(
            set.events(),
            &[DirtyEvent::Renamed {
                from: classified("a.py", PathClass::SOURCE),
                to: classified("a.py.bak", PathClass::OTHER),
            }]
        );
    }

    #[test]
    fn test_without_other_unmerged_csv_dropped_and_unmerged_py_kept() {
        let set = DirtySet::from_porcelain_z(b"UU data.csv\0DD lib.py\0")
            .unwrap()
            .classify(&router(), None)
            .without_other();
        assert_eq!(
            set.events(),
            &[DirtyEvent::Unmerged(classified(
                "lib.py",
                PathClass::SOURCE
            ))]
        );
    }

    #[test]
    fn test_without_other_only_other_paths_returns_empty_events() {
        let set = DirtySet::from_porcelain_z(b"?? a.csv\0?? notes.txt\0?? graph.bin\0")
            .unwrap()
            .classify(&router(), None)
            .without_other();
        assert!(set.events().is_empty());
        assert!(set.is_empty());
    }
}
