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
        let file = File::open(path.as_ref())?;
        let mmap = unsafe { Mmap::map(&file)? };
        validate_header(&mmap)?;
        Ok(Self {
            mmap,
            graph_path: path.as_ref().to_path_buf(),
        })
    }

    pub fn graph(&self) -> Result<&ArchivedZeroCopyGraph, Error> {
        rkyv::access::<ArchivedZeroCopyGraph, Error>(&self.mmap)
    }
}

impl graph_nexus_mcp::registry::EngineRef for Engine {
    fn graph_path(&self) -> &Path {
        &self.graph_path
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

/// Extract the concrete `Engine` from a `&dyn EngineRef`. Returns an
/// `InvalidArgument` error if the underlying object is not an `Engine`
/// (e.g. daemon mode passes `NoopEngine` for commands that don't need
/// a graph). Used by `run_inner` functions whose body needs direct
/// `Engine` access.
pub fn cast_engine(
    engine: &dyn graph_nexus_mcp::registry::EngineRef,
) -> Result<&Engine, graph_nexus_core::GnxError> {
    engine
        .as_any()
        .and_then(|a| a.downcast_ref::<Engine>())
        .ok_or_else(|| {
            graph_nexus_core::GnxError::InvalidArgument("engine not available".to_string())
        })
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
