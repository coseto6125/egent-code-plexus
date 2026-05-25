//! Shared filesystem helpers for host skill-pack install / uninstall.
//! Each per-host module (`codex.rs`, `claude.rs`, …) calls these instead
//! of copy-pasting the recursive directory walk.

use ecp_core::EcpError;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

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

// ── skill diff engine (shared by install + `ecp admin doctor`) ──────────────────────

/// Per-file outcome of comparing a skill's repo source against its installed copy.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileStatus {
    /// In source, not yet installed.
    Added,
    /// Installed, no longer in source.
    Removed,
    /// In both, content differs. `local_edit` marks the case where the
    /// destination already existed before this install (a prior version was
    /// present), so the difference may be a user hand-edit about to be lost.
    Modified {
        unified_diff: String,
        local_edit: bool,
    },
    Unchanged,
}

#[derive(Debug)]
pub(crate) struct FileDiff {
    pub rel_path: String,
    pub status: FileStatus,
}

#[derive(Debug)]
pub(crate) struct SkillDiff {
    pub diffs: Vec<FileDiff>,
}

impl SkillDiff {
    /// True when installing would add, remove, or change at least one file.
    pub fn has_changes(&self) -> bool {
        self.diffs
            .iter()
            .any(|d| !matches!(d.status, FileStatus::Unchanged))
    }

    /// Print the diff to stdout. Unchanged files are omitted. Modified files
    /// carrying a `local_edit` flag are headed with a warning so a user about
    /// to overwrite a hand-edited skill sees it before the copy happens.
    pub fn print(&self) {
        for d in &self.diffs {
            match &d.status {
                FileStatus::Unchanged => {}
                FileStatus::Added => println!("  + {}", d.rel_path),
                FileStatus::Removed => {
                    println!("  - {} (installed copy will be removed)", d.rel_path)
                }
                FileStatus::Modified {
                    unified_diff,
                    local_edit,
                } => {
                    if *local_edit {
                        println!("  ~ {} (local edits will be overwritten)", d.rel_path);
                    } else {
                        println!("  ~ {}", d.rel_path);
                    }
                    for line in unified_diff.lines() {
                        println!("    {line}");
                    }
                }
            }
        }
        if !self.has_changes() {
            println!("  (no changes — installed copy matches source)");
        }
    }
}

/// Compare skill source dir `src` against installed dir `dst`. Pure over the
/// two trees: walks both, pairs files by their path relative to each root, and
/// classifies each. `dst_was_installed` (caller passes `dst/SKILL.md` existence)
/// flags Modified files as potential local edits. Binary files that don't decode
/// as UTF-8 report a placeholder diff rather than line-level text.
pub(crate) fn skill_diff(
    src: &Path,
    dst: &Path,
    dst_was_installed: bool,
) -> Result<SkillDiff, EcpError> {
    let mut rels: BTreeSet<PathBuf> = BTreeSet::new();
    collect_rel_files(src, src, &mut rels)?;
    if dst.exists() {
        collect_rel_files(dst, dst, &mut rels)?;
    }

    let mut diffs = Vec::with_capacity(rels.len());
    for rel in rels {
        let src_file = src.join(&rel);
        let dst_file = dst.join(&rel);
        // Normalize to forward slashes so rel_path is stable across platforms
        // (Windows PathBuf renders `\`, breaking diff headers and lookups).
        let rel_path = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let status = match (src_file.is_file(), dst_file.is_file()) {
            (true, false) => FileStatus::Added,
            (false, true) => FileStatus::Removed,
            (true, true) => {
                let src_bytes = fs::read(&src_file)?;
                let dst_bytes = fs::read(&dst_file)?;
                if src_bytes == dst_bytes {
                    FileStatus::Unchanged
                } else {
                    FileStatus::Modified {
                        unified_diff: unified_diff(&dst_bytes, &src_bytes, &rel_path),
                        local_edit: dst_was_installed,
                    }
                }
            }
            // Neither is a regular file (e.g. dir-only path) — skip.
            (false, false) => continue,
        };
        diffs.push(FileDiff { rel_path, status });
    }
    Ok(SkillDiff { diffs })
}

/// Recursively gather file paths under `root`, each relative to `base`.
fn collect_rel_files(base: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) -> Result<(), EcpError> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rel_files(base, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.insert(rel.to_path_buf());
        }
    }
    Ok(())
}

/// Unified diff `old` → `new`. Falls back to a placeholder for non-UTF-8 bytes.
fn unified_diff(old: &[u8], new: &[u8], rel_path: &str) -> String {
    match (std::str::from_utf8(old), std::str::from_utf8(new)) {
        (Ok(old), Ok(new)) => similar::TextDiff::from_lines(old, new)
            .unified_diff()
            .header(
                &format!("installed/{rel_path}"),
                &format!("source/{rel_path}"),
            )
            .to_string(),
        _ => "<binary file differs>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn status_of<'a>(d: &'a SkillDiff, rel: &str) -> &'a FileStatus {
        &d.diffs.iter().find(|f| f.rel_path == rel).unwrap().status
    }

    #[test]
    fn skill_diff_classifies_added_removed_modified_unchanged() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(src.path(), "SKILL.md", "line a\nline b\n");
        write(dst.path(), "SKILL.md", "line a\nCHANGED\n");
        write(src.path(), "guides/new.md", "fresh\n"); // src-only → Added
        write(dst.path(), "stale.md", "gone\n"); // dst-only → Removed
        write(src.path(), "same.md", "identical\n");
        write(dst.path(), "same.md", "identical\n");

        let d = skill_diff(src.path(), dst.path(), true).unwrap();

        assert!(matches!(status_of(&d, "guides/new.md"), FileStatus::Added));
        assert!(matches!(status_of(&d, "stale.md"), FileStatus::Removed));
        assert!(matches!(status_of(&d, "same.md"), FileStatus::Unchanged));
        match status_of(&d, "SKILL.md") {
            FileStatus::Modified {
                unified_diff,
                local_edit,
            } => {
                assert!(*local_edit, "dst_was_installed=true → local_edit flagged");
                assert!(unified_diff.contains("CHANGED"));
                assert!(unified_diff.contains("line b"));
            }
            other => panic!("expected Modified, got {other:?}"),
        }
        assert!(d.has_changes());
    }

    #[test]
    fn skill_diff_modified_without_prior_install_not_local_edit() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(src.path(), "SKILL.md", "new\n");
        write(dst.path(), "SKILL.md", "old\n");
        let d = skill_diff(src.path(), dst.path(), false).unwrap();
        match status_of(&d, "SKILL.md") {
            FileStatus::Modified { local_edit, .. } => assert!(!*local_edit),
            other => panic!("expected Modified, got {other:?}"),
        }
    }

    #[test]
    fn skill_diff_identical_trees_have_no_changes() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(src.path(), "SKILL.md", "x\n");
        write(dst.path(), "SKILL.md", "x\n");
        let d = skill_diff(src.path(), dst.path(), true).unwrap();
        assert!(!d.has_changes());
    }

    #[test]
    fn skill_diff_missing_dst_marks_all_added() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let missing = dst.path().join("never-installed");
        write(src.path(), "SKILL.md", "x\n");
        let d = skill_diff(src.path(), &missing, false).unwrap();
        assert!(matches!(status_of(&d, "SKILL.md"), FileStatus::Added));
    }
}
