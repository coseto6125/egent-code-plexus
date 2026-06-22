//! Kubernetes `LangSpec` — capture-name → NodeKind table.
//!
//! Note: KubernetesProvider does not use the phf lookup in parser.rs
//! (it performs custom YAML schema walking instead, mirroring
//! DockerComposeProvider). This spec.rs exists for consistency with the
//! LangSpec trait, but the capture_kind table is not consulted during parsing.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct KubernetesSpec;

impl LangSpec for KubernetesSpec {
    const NAME: &'static str = "kubernetes";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {};
}
