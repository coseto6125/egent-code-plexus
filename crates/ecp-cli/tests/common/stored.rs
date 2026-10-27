//! Locators and copy helpers for the checked-in 0.16.4 fixture
//! (`fixtures/stored-by-0.16.4/`). `stored_formats_prev_release.rs` reads each
//! stored format alone; `stored_formats_mixed_cache.rs` combines them in one
//! cache. Both name the writer's identifiers through these consts.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

pub const WRITER_GRAPH_FORMAT: u32 = 15;
pub const WRITER_FINGERPRINT: &str = "v0.16.4+schema20";
pub const WRITER_PARSE_DIR: &str = "568515f1";
pub const COMMIT_DIR: &str = "branch_main__e7ca0e6a663e619e7b245890db6cb47653fed6dc";
pub const SESSION: &str = "fxsession0001";

pub fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stored-by-0.16.4")
}

pub fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

pub fn committed(rel: &str) -> Vec<u8> {
    fs::read(fixture().join("source/committed").join(rel)).unwrap()
}

pub fn set_mtime(path: &Path, mtime_ns: u64) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_nanos(mtime_ns))
        .unwrap();
}
