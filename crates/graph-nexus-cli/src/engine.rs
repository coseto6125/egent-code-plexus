use graph_nexus_core::graph::{ArchivedZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use memmap2::Mmap;
use rkyv::rancor::Error;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

pub struct Engine {
    mmap: Mmap,
    graph_path: PathBuf,
    // Phase 3 reserves the slot; Phase 5 will wire L1 overlay merge into query paths.
    #[allow(dead_code)]
    overlay_dir: Option<PathBuf>,
}

impl Engine {
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        // Canonicalize so callers (especially `index_dir()`) always see an
        // absolute path. The legacy default `.gnx/graph.bin` arrives here
        // relative when `graph_path::resolve` falls through (e.g. cwd is
        // outside any registered repo) — without canonicalize, `index_dir()`
        // would yield `.gnx` and the tantivy lookup would resolve against
        // whatever the process cwd happens to be at search time.
        let graph_path =
            fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf());
        let file = File::open(&graph_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        validate_header(&mmap)?;
        Ok(Self {
            mmap,
            graph_path,
            overlay_dir: None,
        })
    }

    /// Attach an L1 session overlay dir (`~/.gnx/<repo>/sessions/<sid>/`)
    /// to merge dirty graph fragments + tantivy delta over the L2 base.
    /// Phase 3 lands the slot; Phase 5 wires the merge logic into query paths.
    #[allow(dead_code)]
    pub fn with_overlay(mut self, dir: PathBuf) -> Self {
        self.overlay_dir = Some(dir);
        self
    }

    pub fn graph(&self) -> Result<&ArchivedZeroCopyGraph, Error> {
        rkyv::access::<ArchivedZeroCopyGraph, Error>(&self.mmap)
    }

    /// Resolved L2 commit directory: `graph.bin` lives directly inside
    /// `~/.gnx/<repo>/commits/<dirname>/`, so the index dir is the immediate
    /// parent of the graph path. Tantivy and meta.json also live there.
    pub fn index_dir(&self) -> Option<&Path> {
        self.graph_path.parent()
    }

    /// Resolved L1 session overlay dir, set via `with_overlay`. None when
    /// no session is attached (e.g. queries without `--session-id`).
    #[allow(dead_code)]
    pub fn overlay_dir(&self) -> Option<&Path> {
        self.overlay_dir.as_deref()
    }
}

/// Cheap predicate for `auto_ensure`: returns `true` iff `graph.bin`
/// can be memory-mapped and passes magic + version validation. Any
/// I/O / mmap / rkyv access / magic / version failure returns `false`
/// so the caller treats a schema break the same as a stale graph and
/// triggers a clean rebuild — without surfacing `InvalidData` on a
/// CLI upgrade that bumped `GRAPH_FORMAT_VERSION`.
pub fn header_compatible(graph_path: &Path) -> bool {
    let Ok(file) = File::open(graph_path) else {
        return false;
    };
    let Ok(mmap) = (unsafe { Mmap::map(&file) }) else {
        return false;
    };
    validate_header(&mmap).is_ok()
}

/// Reject `graph.bin` files that don't carry the gnx magic header or
/// whose on-disk format version this reader doesn't understand. Both
/// failure modes would otherwise be undetected by `rkyv::access`
/// (which only validates structural layout, not field values) and
/// surface as segfaults or silent misinterpretation downstream.
fn validate_header(bytes: &[u8]) -> io::Result<()> {
    let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("graph.bin: structural validation failed: {e}"),
        )
    })?;
    if archived.magic != GRAPH_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "graph.bin: bad magic — expected {:?}, got {:?}",
                GRAPH_MAGIC, archived.magic
            ),
        ));
    }
    let version = archived.version.to_native();
    if version != GRAPH_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "graph.bin: incompatible format version {version} \
                 (this reader expects {GRAPH_FORMAT_VERSION}) — run `gnx analyze` to regenerate"
            ),
        ));
    }
    Ok(())
}
