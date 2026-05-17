//! Docker Compose `LangSpec` — capture-name → NodeKind table.
//!
//! Note: DockerComposeProvider does not use the phf lookup in parser.rs
//! (it performs custom YAML schema walking instead). This spec.rs exists
//! for consistency with the LangSpec trait, but the capture_kind table
//! is not directly consulted during parsing.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct DockerComposeSpec;

impl LangSpec for DockerComposeSpec {
    const NAME: &'static str = "docker-compose";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {};
}
