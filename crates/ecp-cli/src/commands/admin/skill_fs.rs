//! Shared filesystem helpers for host skill-pack install / uninstall.
//! Each per-host module (`codex.rs`, `claude.rs`, …) calls these instead
//! of copy-pasting the recursive directory walk.

use ecp_core::EcpError;
use std::fs;
use std::path::Path;

pub(crate) fn copy_dir_replace(src: &Path, dst: &Path) -> Result<(), EcpError> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }
    fs::create_dir_all(dst)?;
    copy_dir_contents(src, dst)
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), EcpError> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            fs::create_dir_all(&dst_path)?;
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
