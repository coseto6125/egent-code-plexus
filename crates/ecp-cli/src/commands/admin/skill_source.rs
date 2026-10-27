//! The one skill source: the copy embedded in the running binary.
//!
//! The skill trees are compiled in with `include_dir!`, and every install, link,
//! compare and refresh path reads that copy. No path consults a checkout under
//! `cwd`, so the result never depends on the directory the user stands in.
//!
//! Dev loop: edit `docs/skills/`, run `cargo build`, then run the built binary's
//! install command. [`SkillSource`] owns a `TempDir` for the extracted files.

use crate::commands::admin::skill_fs::copy_dir_replace;
use ecp_core::EcpError;
use include_dir::{include_dir, Dir};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// `docs/skills/ecp/` — the canonical Claude `ecp` skill (also the source of the
/// `ECP.md` guidance import appended to the user's CLAUDE.md).
static ECP_SKILL: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../docs/skills/ecp");
/// `skill_sample/claude/` — bundled Claude skills other than `ecp` (e.g. `simplify`).
static CLAUDE_SAMPLES: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../skill_sample/claude");
/// `skill_sample/codex/` — bundled Codex skills (`ecp`, `simplify`).
static CODEX_SAMPLES: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../skill_sample/codex");

/// Which embedded tree a skill lives under, mirroring the on-disk `docs/` vs
/// `skill_sample/` split. The sample trees carry the skill name so the caller
/// can't pair the wrong tree with a stray path.
#[derive(Clone, Copy)]
pub(crate) enum EmbeddedTree {
    /// `docs/skills/ecp` — the embedded `Dir`'s root *is* the skill.
    EcpSkill,
    /// `skill_sample/claude/<skill>`.
    ClaudeSample(&'static str),
    /// `skill_sample/codex/<skill>`.
    CodexSample(&'static str),
}

impl EmbeddedTree {
    /// The `include_dir!` root tree, plus this skill's path within it. `extract`
    /// reproduces the tree's internal paths, so the skill lands at
    /// `<dest>/<sub>` (empty `sub` for `EcpSkill`, whose root is the skill).
    fn embedded(self) -> (&'static Dir<'static>, &'static str) {
        match self {
            EmbeddedTree::EcpSkill => (&ECP_SKILL, ""),
            EmbeddedTree::ClaudeSample(skill) => (&CLAUDE_SAMPLES, skill),
            EmbeddedTree::CodexSample(skill) => (&CODEX_SAMPLES, skill),
        }
    }
}

/// An extracted skill directory. It owns the `TempDir` the files were extracted
/// into, so the path stays valid for the lifetime of this value.
pub(crate) struct SkillSource {
    path: PathBuf,
    _temp: TempDir,
}

impl SkillSource {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Materialize the embedded copy of the skill within `tree` into a temp dir.
pub(crate) fn embedded(tree: EmbeddedTree) -> Result<SkillSource, EcpError> {
    // Extract the whole embedded tree to a temp dir; `extract` reproduces the
    // tree's internal paths, so the skill lands at `<temp>/<sub>`.
    let (root, sub) = tree.embedded();
    let temp = tempfile::tempdir()
        .map_err(|e| EcpError::Output(format!("create temp dir for embedded skill: {e}")))?;
    root.extract(temp.path())
        .map_err(|e| EcpError::Output(format!("extract embedded skill: {e}")))?;
    let path = if sub.is_empty() {
        temp.path().to_path_buf()
    } else {
        temp.path().join(sub)
    };
    Ok(SkillSource { path, _temp: temp })
}

/// Resolve a skill source to a **persistent** directory, for hosts that link to
/// the source rather than copying it (Gemini's `skills link`). The embedded copy
/// is materialized under `~/.ecp/skills/<skill>` and refreshed on every call — a
/// temp dir would vanish and break the link. Returns the directory path.
pub(crate) fn resolve_persistent(
    tree: EmbeddedTree,
    skill_name: &str,
) -> Result<PathBuf, EcpError> {
    let dest = ecp_core::registry::resolve_home_ecp()
        .join("skills")
        .join(skill_name);
    // `copy_dir_replace` clears `dest` first, so an upgraded binary overwrites a
    // stale materialized copy.
    copy_dir_replace(embedded(tree)?.path(), &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_trees_carry_expected_skills() {
        // The bundled skills must be present in the binary so a package-manager
        // install (no repo tree) can still install them.
        assert!(
            ECP_SKILL.get_file("SKILL.md").is_some(),
            "ecp skill SKILL.md embedded"
        );
        assert!(
            CLAUDE_SAMPLES.get_file("simplify/SKILL.md").is_some(),
            "claude simplify SKILL.md embedded"
        );
        assert!(
            CODEX_SAMPLES.get_file("ecp/SKILL.md").is_some(),
            "codex ecp SKILL.md embedded"
        );
    }

    #[test]
    fn embedded_ecp_skill_materializes_skill_md() {
        let src = embedded(EmbeddedTree::EcpSkill).unwrap();
        assert!(src.path().join("SKILL.md").is_file());
    }

    #[test]
    fn embedded_ecp_skill_returns_bytes_compiled_into_binary() {
        let src = embedded(EmbeddedTree::EcpSkill).unwrap();
        for rel in ["SKILL.md", "ECP.md"] {
            assert_eq!(
                std::fs::read(src.path().join(rel)).unwrap(),
                ECP_SKILL.get_file(rel).unwrap().contents(),
                "{rel}"
            );
        }
    }

    #[test]
    fn embedded_codex_simplify_materializes_skill_md() {
        let src = embedded(EmbeddedTree::CodexSample("simplify")).unwrap();
        assert!(src.path().join("SKILL.md").is_file());
    }

    #[test]
    fn resolve_persistent_materializes_to_home_ecp() {
        // Lands under a persistent `~/.ecp/skills/ecp` (ECP_HOME-scoped here so
        // the test is isolated), not a temp dir that would vanish and break the
        // Gemini link.
        let home = tempfile::tempdir().unwrap();
        let _guard = EcpHomeGuard::set(home.path());
        let path = resolve_persistent(EmbeddedTree::EcpSkill, "ecp").unwrap();
        assert!(path.join("SKILL.md").is_file());
        assert!(path.starts_with(home.path()));
    }

    #[test]
    fn resolve_persistent_twice_refreshes_without_error() {
        let home = tempfile::tempdir().unwrap();
        let _guard = EcpHomeGuard::set(home.path());
        let first = resolve_persistent(EmbeddedTree::EcpSkill, "ecp").unwrap();
        std::fs::write(first.join("stale.txt"), "x").unwrap();
        let second = resolve_persistent(EmbeddedTree::EcpSkill, "ecp").unwrap();
        assert_eq!(first, second);
        assert!(!second.join("stale.txt").exists());
        assert!(second.join("SKILL.md").is_file());
    }

    /// Scope an `ECP_HOME` override for one test, restoring the prior value on
    /// drop. Serialized via a mutex because env is process-global.
    struct EcpHomeGuard {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl EcpHomeGuard {
        fn set(path: &Path) -> Self {
            static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("ECP_HOME");
            std::env::set_var("ECP_HOME", path);
            Self { prev, _lock: lock }
        }
    }
    impl Drop for EcpHomeGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("ECP_HOME", v),
                None => std::env::remove_var("ECP_HOME"),
            }
        }
    }
}
