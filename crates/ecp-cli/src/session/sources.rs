//! The overlay view's source reader (`ecp_core::session::view::SourceReader`),
//! built from the inputs whose hashes the view cache key covers.

use super::overlay_reader::ViewInputs;
use ecp_core::graph::ArchivedZeroCopyGraph;
use rustc_hash::FxHashMap;
use std::cell::Cell;
use std::fs;
use std::path::Path;

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
        let recorded = match self.dirty.get(rel) {
            Some(&hash) => hash,
            None => {
                let file = &self.graph.files[self.graph.file_idx_of(rel)? as usize];
                if file.content_hash == [0; 8] {
                    return None;
                }
                u64::from_le_bytes(file.content_hash)
            }
        };
        let bytes = fs::read(self.root.join(rel))
            .ok()
            .filter(|bytes| xxhash_rust::xxh3::xxh3_64(bytes) == recorded);
        let Some(bytes) = bytes else {
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
