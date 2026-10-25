//! The one read path of a graph query (architecture direction, area 5): the
//! directory a query names becomes the worktree top, the graph is ensured and
//! loaded, the session overlay is attached, and the dirty set and the caveats
//! travel with the engine.
//!
//! Everything is rooted at the worktree top because `git status --porcelain`
//! names paths relative to it, and the overlay's fragments and their mtime
//! checks are keyed the same way. Rooted at a subdirectory, the gate joined
//! `pkg/sub/a.py` onto `pkg/sub`, parsed nothing, and every uncommitted symbol
//! read as absent.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use ecp_core::contracts::dirty_event::{ClassifiedPath, DirtySet, SkipReason};
use ecp_core::contracts::identity::{
    self, Ambiguity, Candidate, CandidateFacts, PathSegment, TargetRequest, TargetResolution,
};
use ecp_core::contracts::routing::SizeCap;
use ecp_core::session::{MergedGraph, MergedNode};
use ecp_core::EcpError;
use serde_json::{json, Value};

use crate::auto_ensure::{self, EnsureFreshOutcome, Ensured, IndexNeed};
use crate::commands::format::node_kind_to_str;
use crate::engine::Engine;
use crate::graph_path;

/// What a graph query reads.
pub struct ReadContext {
    /// The nearest ancestor of the queried directory that holds `.git`. The
    /// queried directory itself when no git layout answers: outside every
    /// repository, in a bare repository, or under `GIT_DIR` and its kin.
    pub worktree_top: PathBuf,
    /// The graph the engine maps: a sibling commit's graph on a warm attach.
    pub graph_path: PathBuf,
    /// The graph, with this worktree's session overlay attached when one
    /// exists and the graph is this worktree's own.
    pub engine: Engine,
    /// The source and config changes the freshness gate saw, relative to
    /// `worktree_top`; empty when it saw none. `None` when no gate named the
    /// paths: a full build or a warm attach answered instead.
    pub dirty: Option<DirtySet<ClassifiedPath>>,
}

/// Why no read context was built. Each caller words it for its own reader.
#[derive(Debug)]
pub enum ReadContextError {
    /// The freshness gate, or the build it started, failed.
    Ensure(String),
    /// The graph the gate settled on did not load.
    Load {
        graph_path: PathBuf,
        warm_attach: bool,
        error: io::Error,
    },
}

impl fmt::Display for ReadContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ensure(e) => f.write_str(e),
            Self::Load {
                graph_path,
                warm_attach: false,
                error,
            } => write!(f, "load graph {}: {error}", graph_path.display()),
            Self::Load {
                graph_path,
                warm_attach: true,
                error,
            } => write!(
                f,
                "load warm-attach graph {}: {error}",
                graph_path.display()
            ),
        }
    }
}

impl ReadContext {
    /// The context of a query that names a directory (the cwd or `--repo`)
    /// and the `--graph` argument. A graph that `--graph` names and that is
    /// not this worktree's own reads no overlay.
    pub fn for_dir(dir: &Path, graph_arg: &Path) -> Result<Self, ReadContextError> {
        let top = worktree_top(dir);
        let resolved = graph_path::resolve(graph_arg, &top);
        let Ensured {
            outcome,
            dirty,
            size_cap,
            ..
        } = auto_ensure::ensure_fresh_with_rebuild(IndexNeed::NearCurrent, &resolved, &top)
            .map_err(ReadContextError::Ensure)?;
        let (engine, graph_path) = match outcome {
            EnsureFreshOutcome::WarmAttach { sibling_graph_path } => {
                (load(&sibling_graph_path, true)?, sibling_graph_path)
            }
            // A build publishes a new commit dir; resolving again reads it.
            EnsureFreshOutcome::Ready => {
                let path = graph_path::resolve(graph_arg, &top);
                (load(&path, false)?, path)
            }
        };
        // The overlay's symbols and edges belong to this worktree's graph, so
        // merging them into a foreign graph answers with rows it does not
        // hold. `--graph` naming the file `resolve` picks anyway is not
        // foreign.
        let own_graph =
            !graph_path::is_custom(graph_arg) || graph_path::is_cwd_graph(&graph_path, &top);
        Ok(Self::assemble(
            top, graph_path, engine, dirty, size_cap, own_graph,
        ))
    }

    /// The context of a graph resolved by repository (a registry target, a
    /// group member, `peers plan`) for the worktree at `worktree_root`. The
    /// engine flags a graph that predates HEAD and starts its rebuild.
    pub fn for_graph(graph_path: &Path, worktree_root: &Path) -> Result<Self, ReadContextError> {
        let top = worktree_top(worktree_root);
        let Ensured {
            outcome,
            rebuilt,
            dirty,
            size_cap,
        } = auto_ensure::ensure_fresh_with_rebuild(IndexNeed::NearCurrent, graph_path, &top)
            .map_err(ReadContextError::Ensure)?;
        let (engine, graph_path) = match outcome {
            EnsureFreshOutcome::WarmAttach { sibling_graph_path } => {
                (load(&sibling_graph_path, true)?, sibling_graph_path)
            }
            EnsureFreshOutcome::Ready => {
                let path = rebuilt.unwrap_or_else(|| graph_path.to_path_buf());
                let mut engine = load(&path, false)?;
                auto_ensure::flag_behind_head(&mut engine, &path, &top);
                (engine, path)
            }
        };
        Ok(Self::assemble(
            top, graph_path, engine, dirty, size_cap, true,
        ))
    }

    fn assemble(
        top: PathBuf,
        graph_path: PathBuf,
        engine: Engine,
        dirty: Option<DirtySet<ClassifiedPath>>,
        size_cap: Option<SizeCap>,
        own_graph: bool,
    ) -> Self {
        let overlay = own_graph
            .then(|| auto_ensure::resolve_session_overlay_dir(&top))
            .flatten()
            .filter(|dir| dir.is_dir());
        let mut engine = match overlay {
            Some(dir) => engine.with_overlay(dir, top.clone(), size_cap),
            None => engine,
        };
        engine.set_unreadable_changes(unreadable_changes(dirty.as_ref()));
        Self {
            worktree_top: top,
            graph_path,
            engine,
            dirty,
        }
    }
}

/// Changed paths git printed that the dirty set cannot name, directories aside.
fn unreadable_changes(dirty: Option<&DirtySet<ClassifiedPath>>) -> usize {
    dirty.map_or(0, |dirty| {
        dirty
            .skipped()
            .iter()
            .filter(|entry| entry.reason != SkipReason::Directory)
            .count()
    })
}

/// The worktree top of `dir`, from the process-wide git layout cache: no git
/// process runs. `dir` itself when the layout declines (see
/// [`ReadContext::worktree_top`]), which keeps the behaviour from before.
pub fn worktree_top(dir: &Path) -> PathBuf {
    crate::git_cache::git_layout(dir).map_or_else(
        || dir.to_path_buf(),
        |(top, _, _)| dunce::simplified(&top).to_path_buf(),
    )
}

// ── The target resolver ─────────────────────────────────────────────────────
//
// `impact`, `inspect` and `rename` turn the target text and its filters into
// one declaration or a typed ambiguity here, and nowhere else (C1 "Target
// request syntax", "Typed target result").

/// The target text with the `--kind` and `--file` filters (C1 grammar). A
/// blank filter counts as absent.
pub fn target_request(
    text: &str,
    kind: Option<&str>,
    file: Option<&str>,
) -> Result<TargetRequest, EcpError> {
    fn present(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    TargetRequest::parse(text, present(kind), present(file)).map_err(|e| {
        EcpError::InvalidArgument(format!(
            "target {text:?}: {e}. Write a name that holds `.`, `::`, `<`, `>` or `@` \
             in backticks, for example `` `<anonymous:3:4>` ``"
        ))
    })
}

/// Every declaration named `name` in merged space: the visible base nodes in
/// node order, then the overlay's virtual nodes in view order. A base node the
/// overlay replaced or deleted is not visible; its virtual successor, if any,
/// answers instead. Synthetic nodes have no file and line, so they are not
/// declarations. A later overload is a declaration like any other.
pub fn named_declarations<'a>(merged: &MergedGraph<'a>, name: &str) -> Vec<CandidateFacts<'a>> {
    let mut out: Vec<CandidateFacts<'a>> = merged
        .name_candidates(name)
        .filter_map(|i| {
            let node = merged.node(i)?;
            let file = node.file_path(merged)?;
            (node.name(merged) == name && merged.base_visible(i))
                .then(|| facts(merged, i, node, file))
        })
        .collect();
    if let Some(view) = merged.view() {
        // A file that defines one symbol twice yields two virtual nodes under
        // one uid; the index keeps only the first.
        let mut seen = rustc_hash::FxHashSet::default();
        out.extend(
            (merged.base_len()..)
                .zip(view.virtual_nodes())
                .filter(|(_, n)| n.name == name && seen.insert(n.uid))
                .map(|(i, n)| facts(merged, i, MergedNode::Virtual(n), &n.rel_path)),
        );
    }
    out
}

fn facts<'a>(
    merged: &MergedGraph<'a>,
    idx: u32,
    node: MergedNode<'a>,
    file: &'a str,
) -> CandidateFacts<'a> {
    CandidateFacts {
        node: idx,
        kind: node.kind(),
        file,
        line: node.start_line(),
        name: node.name(merged),
        qualifier: node
            .owner_class(merged)
            .map(PathSegment::from_owner)
            .unwrap_or_default(),
    }
}

/// The one name-to-node lookup of a graph command.
pub fn resolve_target(merged: &MergedGraph<'_>, request: &TargetRequest) -> TargetResolution {
    identity::resolve_target(request, &named_declarations(merged, request.name()))
}

/// The candidates of an ambiguity as a payload reads them. `target` is the
/// command arguments that select that candidate alone, `null` when none can.
pub fn ambiguity_candidates(ambiguity: &Ambiguity) -> Value {
    ambiguity
        .candidates()
        .iter()
        .map(|c| candidate_json(c, c.reusable.as_ref().map(target_args)))
        .collect()
}

pub fn candidate_json(c: &Candidate, target: Option<String>) -> Value {
    json!({
        "filePath": c.file,
        "kind": node_kind_to_str(&c.kind),
        "line": c.line,
        "target": target,
    })
}

/// A request as command arguments: the target text, then `--file` and
/// `--kind` when the request needs them.
pub fn target_args(request: &TargetRequest) -> String {
    let mut out = shell_word(&request.render());
    if let Some(file) = request.file() {
        out.push_str(" --file ");
        out.push_str(&shell_word(file));
    }
    if let Some(kind) = request.kind() {
        out.push_str(" --kind ");
        out.push_str(&node_kind_to_str(&kind).to_ascii_lowercase());
    }
    out
}

/// A target text, as one shell word, that selects `c` alone among `named`
/// with no `--file` and no `--kind`, for a command whose filters do not
/// narrow the target (`inspect`): the reusable request when it needs neither,
/// else the qualified name at its line. `None` when the text alone cannot
/// separate it.
pub fn text_only_target(c: &Candidate, named: &[CandidateFacts<'_>]) -> Option<String> {
    if let Some(r) = c
        .reusable
        .as_ref()
        .filter(|r| r.file().is_none() && r.kind().is_none())
    {
        return Some(shell_word(&r.render()));
    }
    let text = format!("{}@{}", c.display, c.line);
    let request = TargetRequest::parse(&text, None, None).ok()?;
    match identity::select(&request, named).as_slice() {
        [one] if named[*one].node == c.node => Some(shell_word(&text)),
        _ => None,
    }
}

/// `word` as one shell word: single-quoted when it is empty or holds a
/// character outside the safe set, with embedded `'` written as `'\''`.
pub(crate) fn shell_word(word: &str) -> String {
    let safe = |c: char| c.is_alphanumeric() || "_-./:@%+=,".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

fn load(path: &Path, warm_attach: bool) -> Result<Engine, ReadContextError> {
    let loaded = match warm_attach {
        true => Engine::load_warm(path),
        false => Engine::load(path),
    };
    loaded.map_err(|error| ReadContextError::Load {
        graph_path: path.to_path_buf(),
        warm_attach,
        error,
    })
}
