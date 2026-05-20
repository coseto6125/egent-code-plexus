//! GitHub Actions `LangSpec` — capture-name → NodeKind table.
//!
//! Note: GitHubActionsProvider does not use the phf lookup in parser.rs
//! (it performs custom YAML schema walking instead). This spec.rs exists
//! for consistency with the LangSpec trait, but the capture_kind table
//! is not directly consulted during parsing.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct GithubActionsSpec;

impl LangSpec for GithubActionsSpec {
    const NAME: &'static str = "github-actions";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {};
}
