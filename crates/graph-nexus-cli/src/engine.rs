use graph_nexus_core::graph::{ArchivedZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use memmap2::Mmap;
use rkyv::rancor::Error;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

pub struct Engine {
    mmap: Mmap,
    graph_path: PathBuf,
}

impl Engine {
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let graph_path = path.as_ref().to_path_buf();
        let file = File::open(&graph_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        validate_header(&mmap)?;
        Ok(Self { mmap, graph_path })
    }

    pub fn graph(&self) -> Result<&ArchivedZeroCopyGraph, Error> {
        rkyv::access::<ArchivedZeroCopyGraph, Error>(&self.mmap)
    }

    /// Repo root for tantivy lookup: `graph.bin` lives at
    /// `<repo>/.gitnexus-rs/graph.bin`, so the root is two parents up.
    pub fn repo_root(&self) -> Option<&Path> {
        self.graph_path.parent()?.parent()
    }
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
