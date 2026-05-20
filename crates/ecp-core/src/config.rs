//! User-facing configuration loaded from `<repo>/.ecp/config.toml`.
//!
//! Each field is documented with its **wiring status**:
//! - `effective` — the rest of the codebase reads this and respects it
//! - `stored`    — value persists across runs but isn't consulted yet
//!
//! All fields default if absent so partial configs are valid.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub confidence: ConfidenceConfig,
    #[serde(default)]
    pub group: GroupConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutputConfig {
    /// **stored** — preferred default format for read commands when
    /// `--format` is omitted. Read commands today have hard-coded
    /// per-command defaults (`toon` for most, `text` for `query`, `md`
    /// for `summarize`). Wiring lands when commands consult this value.
    #[serde(default = "default_output_format")]
    pub default_format: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            default_format: default_output_format(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfidenceConfig {
    /// **stored** — override for `HIGH_TRUST_CONFIDENCE` (currently the
    /// const `0.8`). Wiring lands when `impact` / `detect_changes` load
    /// the config and pass this value down instead of the const.
    #[serde(default = "default_high_trust_threshold")]
    pub high_trust_threshold: f32,
}

impl Default for ConfidenceConfig {
    fn default() -> Self {
        Self {
            high_trust_threshold: default_high_trust_threshold(),
        }
    }
}

fn default_output_format() -> String {
    "toon".to_string()
}
fn default_high_trust_threshold() -> f32 {
    crate::HIGH_TRUST_CONFIDENCE
}

/// **stored** — values consumed by `ecp group sync / impact` when
/// CLI flags do not override. See
/// `docs/specs/2026-05-18-ecp-group-multirepo-design.md` §Configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupConfig {
    #[serde(default = "default_group_bm25_threshold")]
    pub bm25_threshold: f32,
    #[serde(default = "default_group_max_candidates")]
    pub max_candidates_per_step: u32,
    #[serde(default)]
    pub exclude_links_paths: Vec<String>,
    #[serde(default)]
    pub exclude_links_param_only_paths: bool,
    #[serde(default = "default_group_cross_depth")]
    pub cross_depth: u32,
    /// 0 disables the timeout (impact runs to completion).
    #[serde(default = "default_group_timeout_ms")]
    pub local_impact_timeout_ms: u64,
}

impl Default for GroupConfig {
    fn default() -> Self {
        Self {
            bm25_threshold: default_group_bm25_threshold(),
            max_candidates_per_step: default_group_max_candidates(),
            exclude_links_paths: Vec::new(),
            exclude_links_param_only_paths: false,
            cross_depth: default_group_cross_depth(),
            local_impact_timeout_ms: default_group_timeout_ms(),
        }
    }
}

fn default_group_bm25_threshold() -> f32 {
    0.6
}
fn default_group_max_candidates() -> u32 {
    16
}
fn default_group_cross_depth() -> u32 {
    1
}
fn default_group_timeout_ms() -> u64 {
    5000
}

/// Repo-relative config path. `.ecp/config.toml` is hook-local state
/// scoped to the worktree (not shared with `~/.ecp/<repo>/<branch>/`,
/// which holds the resolved index artifacts).
pub fn config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".ecp").join("config.toml")
}

/// Load the config from `<repo>/.ecp/config.toml`. Returns
/// `Config::default()` if the file is absent (first-run case) so callers
/// can `unwrap_or_default()` without branching on the missing file.
pub fn load(repo_root: &Path) -> Result<Config, String> {
    let path = config_path(repo_root);
    if !path.exists() {
        return Ok(Config::default());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| format!("utf-8 {}: {e}", path.display()))?;
    toml::from_str::<Config>(text).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Atomic write to `<repo>/.ecp/config.toml` (tmp + fsync +
/// rename — same pattern as the registry / graph.bin writes).
pub fn save(repo_root: &Path, cfg: &Config) -> Result<(), String> {
    let path = config_path(repo_root);
    let text = toml::to_string_pretty(cfg).map_err(|e| format!("serialize: {e}"))?;
    crate::registry::atomic_write_bytes(&path, text.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_round_trip_through_toml() {
        let cfg = Config::default();
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn load_returns_defaults_when_file_missing() {
        let dir = tempdir().unwrap();
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.output.default_format = "json".into();
        cfg.confidence.high_trust_threshold = 0.7;
        save(dir.path(), &cfg).unwrap();
        let back = load(dir.path()).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn partial_toml_uses_defaults_for_missing_sections() {
        let dir = tempdir().unwrap();
        let cfg_path = config_path(dir.path());
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(&cfg_path, "[output]\ndefault_format = \"json\"\n").unwrap();
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.output.default_format, "json");
        assert_eq!(cfg.confidence, ConfidenceConfig::default());
    }
}
