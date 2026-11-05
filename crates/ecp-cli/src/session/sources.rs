//! The overlay view's source reader (`ecp_core::session::view::SourceReader`),
//! built from the inputs whose hashes the view cache key covers.

use super::overlay_reader::ViewInputs;
use ecp_core::config::{resolve_size_cap, DEFAULT_MAX_FILE_BYTES};
use ecp_core::contracts::routing::SizeCap;
use ecp_core::graph::ArchivedZeroCopyGraph;
use rustc_hash::FxHashMap;
use std::cell::Cell;
use std::fs;
use std::io::Read;
use std::path::Path;
use xxhash_rust::xxh3::xxh3_64;

/// Gives a file's text only when the text's `xxh3_64` is the hash the
/// view's inputs recorded for the path. A dirty file's recorded hash is its
/// accepted fragment's (`AcceptedFragment::content_hash`); any other
/// file's is `graph.bin`'s `File.content_hash` (the `u64`'s little-endian
/// bytes, all zero for a file the index omitted, which records none).
pub struct RecordedSources<'g> {
    root: &'g Path,
    graph: &'g ArchivedZeroCopyGraph,
    /// Each accepted fragment's path → the content hash it parsed.
    dirty: FxHashMap<&'g str, u64>,
    /// Set once a read found no file, or text other than the recorded one,
    /// at a recorded hash.
    unrecorded: Cell<bool>,
}

impl<'g> RecordedSources<'g> {
    /// The reader over `root` for the view built from `graph` and `inputs`.
    pub fn new(root: &'g Path, graph: &'g ArchivedZeroCopyGraph, inputs: &'g ViewInputs) -> Self {
        let dirty = inputs
            .fragments
            .iter()
            .filter_map(|fragment| Some((fragment.rel_path.as_str(), fragment.content_hash?)))
            .collect();
        Self {
            root,
            graph,
            dirty,
            unrecorded: Cell::new(false),
        }
    }

    /// The text of `rel` when it is the recorded one. `None` for a path no
    /// input records, for a zero recorded hash, and for a file that is gone,
    /// unreadable, of other text, or not UTF-8.
    pub fn read(&self, rel: &str) -> Option<String> {
        let recorded = recorded_hash(self.graph, &self.dirty, rel)?;
        let Some(bytes) = recorded_bytes(self.root, rel, recorded) else {
            self.unrecorded.set(true);
            return None;
        };
        String::from_utf8(bytes).ok()
    }

    /// Whether every read at a recorded hash found the recorded text: what
    /// the view read is then fixed by its inputs.
    pub fn all_recorded(&self) -> bool {
        !self.unrecorded.get()
    }
}

/// The hash `rel`'s text must have: the accepted fragment's in `dirty`, else
/// `graph.bin`'s `File.content_hash`. `None` for a path the graph does not
/// list and for a zero graph hash, which records nothing.
pub fn recorded_hash<K>(
    graph: &ArchivedZeroCopyGraph,
    dirty: &FxHashMap<K, u64>,
    rel: &str,
) -> Option<u64>
where
    K: std::borrow::Borrow<str> + std::hash::Hash + Eq,
{
    if let Some(&hash) = dirty.get(rel) {
        return Some(hash);
    }
    let file = &graph.files[graph.file_idx_of(rel)? as usize];
    (file.content_hash != [0; 8]).then(|| u64::from_le_bytes(file.content_hash))
}

/// The bytes of `root/rel` when their `xxh3_64` is `recorded`. `None` for a
/// file that is gone or unreadable: a missing file proves nothing, not even
/// an empty recorded text. `None` also for a path that [`open_admitted`]
/// refuses, and for a file whose length changed while it was read.
pub fn recorded_bytes(root: &Path, rel: &str, recorded: u64) -> Option<Vec<u8>> {
    let (file, len) = open_admitted(&root.join(rel), || resolve_size_cap(root))?;
    let mut bytes = Vec::with_capacity(usize::try_from(len).ok()?);
    file.take(len + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 == len && xxh3_64(&bytes) == recorded).then_some(bytes)
}

/// Whether `root/rel` holds the text whose `xxh3_64` is `recorded`, hashed in
/// a stream so the file is never loaded whole. The same paths as
/// [`recorded_bytes`] prove nothing. `cap` gives the parse cap; it runs only
/// for a file over the built-in default.
pub fn text_is_recorded(
    root: &Path,
    rel: &str,
    recorded: u64,
    cap: impl FnOnce() -> SizeCap,
) -> bool {
    let Some((file, len)) = open_admitted(&root.join(rel), cap) else {
        return false;
    };
    // At most one byte past the length the handle reported, so a file that
    // grows while it is hashed is read no further and proves nothing.
    let mut reader = file.take(len + 1);
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut buf = [0u8; 64 * 1024];
    let mut read = 0u64;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                read += n as u64;
                hasher.update(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    read == len && hasher.digest() == recorded
}

/// `path` open for reading, with its length, when [`admitted_len`] admits it
/// and the open handle is still a regular file of that length. The type and
/// length are read again from the handle, so a path swapped for a FIFO or a
/// larger file after the `stat` is judged as what was opened; the open does
/// not block, so a FIFO put in its place is refused, not waited on.
fn open_admitted(path: &Path, cap: impl FnOnce() -> SizeCap) -> Option<(fs::File, u64)> {
    let stat_len = admitted_len(path, cap)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let len = file.metadata().ok().filter(fs::Metadata::is_file)?.len();
    (len == stat_len).then_some((file, len))
}

/// The length of the file at `path` when it is a regular file (symlinks
/// followed) that a parse admits. Nothing else is opened: opening a FIFO
/// blocks, and a device such as `/dev/zero` never ends. A file at most the
/// built-in default is admitted without resolving the cap, so the common case
/// costs one `stat` and no config read; any file this admits is therefore at
/// most the larger of the two caps.
fn admitted_len(path: &Path, cap: impl FnOnce() -> SizeCap) -> Option<u64> {
    let meta = fs::metadata(path).ok().filter(fs::Metadata::is_file)?;
    let len = meta.len();
    (len <= DEFAULT_MAX_FILE_BYTES || cap().admits(len)).then_some(len)
}
