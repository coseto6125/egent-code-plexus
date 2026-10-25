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
use ecp_core::contracts::routing::SizeCap;

use crate::auto_ensure::{self, EnsureFreshOutcome, Ensured, IndexNeed};
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
