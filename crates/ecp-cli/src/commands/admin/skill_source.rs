//! Skill-pack source resolution that works both inside the ecp repo and from a
//! binary-only install (`npx` / `uvx` / `cargo binstall`).
//!
//! The install / diff / doctor flows are all path-based (`skill_fs.rs`), so the
//! historical source was `cwd.join("docs/skills/…")` — fine when run from a repo
//! checkout, but a binary installed via a package manager ships no source tree,
//! leaving `ecp admin … install skills` with nowhere to read from.
//!
//! Fix: embed the canonical skill trees into the binary with `include_dir!`, and
//! resolve a source root with this priority:
//!   1. the repo checkout under `cwd` (so editing `docs/skills/` then reinstalling
//!      takes effect immediately — preserves the repo dev loop), else
//!   2. a temp dir materialized from the embedded copy (the binary-only case).
//!
//! Materializing reuses the existing path-based copy/diff engine unchanged: the
//! returned [`SkillSource`] owns a `TempDir` when embedded, keeping the files
//! alive until the caller is done.

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
    /// Repo-relative path (from repo root) for this skill.
    fn repo_relative(self) -> PathBuf {
        match self {
            EmbeddedTree::EcpSkill => PathBuf::from("docs").join("skills").join("ecp"),
            EmbeddedTree::ClaudeSample(skill) => {
                PathBuf::from("skill_sample").join("claude").join(skill)
            }
            EmbeddedTree::CodexSample(skill) => {
                PathBuf::from("skill_sample").join("codex").join(skill)
            }
        }
    }

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

/// A resolved skill-source directory. When sourced from the embedded copy it owns
/// the `TempDir` the files were extracted into, so the path stays valid for the
/// lifetime of this value.
pub(crate) struct SkillSource {
    path: PathBuf,
    _temp: Option<TempDir>,
}

impl SkillSource {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Resolve the source directory for a skill within `tree`, rooted at `cwd`.
/// Repo checkout under `cwd` wins; otherwise the embedded copy is materialized
/// into a temp dir.
pub(crate) fn resolve(tree: EmbeddedTree, cwd: &Path) -> Result<SkillSource, EcpError> {
    let repo_path = cwd.join(tree.repo_relative());
    if repo_path.join("SKILL.md").is_file() {
        return Ok(SkillSource {
            path: repo_path,
            _temp: None,
        });
    }

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
    Ok(SkillSource {
        path,
        _temp: Some(temp),
    })
}

/// Resolve a skill source to a **persistent** directory, for hosts that link to
/// the source rather than copying it (Gemini's `skills link`). Repo checkout
/// under `cwd` wins; otherwise the embedded copy is materialized once under
/// `~/.ecp/skills/<skill>` and reused — a temp dir would vanish and break the
/// link. Returns the directory path.
pub(crate) fn resolve_persistent(
    tree: EmbeddedTree,
    skill_name: &str,
    cwd: &Path,
) -> Result<PathBuf, EcpError> {
    let repo_path = cwd.join(tree.repo_relative());
    if repo_path.join("SKILL.md").is_file() {
        return Ok(repo_path);
    }

    let dest = ecp_core::registry::resolve_home_ecp()
        .join("skills")
        .join(skill_name);
    // Refresh so an upgraded binary overwrites a stale materialized copy.
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| {
            EcpError::Output(format!("clear stale skill dir {}: {e}", dest.display()))
        })?;
    }
    let (root, sub) = tree.embedded();
    if sub.is_empty() {
        // `extract` writes into an existing dir; create the skill dir itself.
        std::fs::create_dir_all(&dest)
            .map_err(|e| EcpError::Output(format!("create {}: {e}", dest.display())))?;
        root.extract(&dest)
            .map_err(|e| EcpError::Output(format!("extract embedded skill: {e}")))?;
    } else {
        // Extract the tree to a temp dir, then move just the `sub` subtree into
        // place so `dest` is the skill itself, not `<root>/<sub>`.
        let temp =
            tempfile::tempdir().map_err(|e| EcpError::Output(format!("create temp dir: {e}")))?;
        root.extract(temp.path())
            .map_err(|e| EcpError::Output(format!("extract embedded skill: {e}")))?;
        copy_dir_replace(&temp.path().join(sub), &dest)?;
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cwd whose repo layout has the `ecp` skill, so `resolve*` returns the
    /// repo path rather than the embedded copy.
    fn fake_repo_with_ecp_skill(body: &str) -> tempfile::TempDir {
        let cwd = tempfile::tempdir().unwrap();
        let src = cwd.path().join("docs").join("skills").join("ecp");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), body).unwrap();
        cwd
    }

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
    fn resolve_prefers_repo_tree_over_embedded() {
        // A cwd whose repo layout has the skill → that path is returned verbatim,
        // not a temp dir (repo dev loop: edits take effect without rebuild).
        let cwd = tempfile::tempdir().unwrap();
        let repo_ecp = cwd.path().join("docs").join("skills").join("ecp");
        std::fs::create_dir_all(&repo_ecp).unwrap();
        std::fs::write(repo_ecp.join("SKILL.md"), "repo copy\n").unwrap();

        let src = resolve(EmbeddedTree::EcpSkill, cwd.path()).unwrap();
        assert_eq!(src.path(), repo_ecp);
        assert_eq!(
            std::fs::read_to_string(src.path().join("SKILL.md")).unwrap(),
            "repo copy\n"
        );
    }

    #[test]
    fn resolve_falls_back_to_embedded_when_no_repo_tree() {
        // A cwd with no repo layout (the npx/uvx case) → embedded copy is
        // materialized and its SKILL.md is readable.
        let cwd = tempfile::tempdir().unwrap();
        let src = resolve(EmbeddedTree::EcpSkill, cwd.path()).unwrap();
        assert_ne!(src.path(), cwd.path().join("docs/skills/ecp"));
        assert!(src.path().join("SKILL.md").is_file());
    }

    #[test]
    fn resolve_codex_simplify_from_embedded() {
        let cwd = tempfile::tempdir().unwrap();
        let src = resolve(EmbeddedTree::CodexSample("simplify"), cwd.path()).unwrap();
        assert!(src.path().join("SKILL.md").is_file());
    }

    #[test]
    fn resolve_persistent_prefers_repo_tree() {
        // Repo tree present → returned verbatim, so Gemini links to the live
        // checkout (edits show up without rebuild).
        let cwd = fake_repo_with_ecp_skill("repo\n");
        let path = resolve_persistent(EmbeddedTree::EcpSkill, "ecp", cwd.path()).unwrap();
        assert_eq!(path, cwd.path().join("docs/skills/ecp"));
    }

    #[test]
    fn resolve_persistent_materializes_to_home_ecp_when_no_repo() {
        // No repo tree (npx/uvx) → embedded copy lands under a persistent
        // `~/.ecp/skills/ecp` (ECP_HOME-scoped here so the test is isolated),
        // not a temp dir that would vanish and break the Gemini link.
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = EcpHomeGuard::set(home.path());
        let path = resolve_persistent(EmbeddedTree::EcpSkill, "ecp", cwd.path()).unwrap();
        assert!(path.join("SKILL.md").is_file());
        assert!(path.starts_with(home.path()));
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
