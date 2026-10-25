//! Where a session keeps its dirty manifest and fragment bins
//! (`docs/specs/contracts/C3-dirty-event.md`, "Manifest isolation").
//!
//! Today's binary writes and reads `l1/` only. The legacy paths are the
//! namespace an older binary of the same fingerprint still writes: `admin gc`
//! sweeps it, and old-data tests read it. No query path opens them.

use std::path::{Path, PathBuf};

const L1_DIR: &str = "l1";

/// `<session>/l1/dirty_events.json`: the [`super::DirtyEvents`] manifest.
pub fn dirty_events_path(session_dir: &Path) -> PathBuf {
    session_dir.join(L1_DIR).join("dirty_events.json")
}

/// `<session>/l1/fragments`: the bins the manifest names.
pub fn fragments_dir(session_dir: &Path) -> PathBuf {
    session_dir.join(L1_DIR).join("fragments")
}

/// `<session>/l1/fragments/<id>.bin`.
pub fn fragment_path(session_dir: &Path, id: &str) -> PathBuf {
    fragments_dir(session_dir).join(format!("{id}.bin"))
}

/// `<session>/dirty_files.json`: the legacy [`super::DirtyFiles`] manifest.
pub fn legacy_dirty_files_path(session_dir: &Path) -> PathBuf {
    session_dir.join("dirty_files.json")
}

/// `<session>/graph_overlay`: the legacy bins directory.
pub fn legacy_overlay_dir(session_dir: &Path) -> PathBuf {
    session_dir.join("graph_overlay")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layout_paths_new_and_legacy_namespaces_never_share_a_path() {
        let s = Path::new("/s");
        assert_eq!(dirty_events_path(s), Path::new("/s/l1/dirty_events.json"));
        assert_eq!(fragments_dir(s), Path::new("/s/l1/fragments"));
        assert_eq!(fragment_path(s, "ab"), Path::new("/s/l1/fragments/ab.bin"));
        assert_eq!(legacy_dirty_files_path(s), Path::new("/s/dirty_files.json"));
        assert_eq!(legacy_overlay_dir(s), Path::new("/s/graph_overlay"));
        assert!(!fragment_path(s, "ab").starts_with(legacy_overlay_dir(s)));
    }
}
