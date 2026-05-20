//! Dockerfile `LangSpec` — capture-name → NodeKind table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct DockerfileSpec;

impl LangSpec for DockerfileSpec {
    const NAME: &'static str = "dockerfile";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "const.name" => NodeKind::Const,
        "arg.name"   => NodeKind::Const,
    };
}
