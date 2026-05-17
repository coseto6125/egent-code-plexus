//! Bash `LangSpec` — capture-name → NodeKind table.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct BashSpec;

impl LangSpec for BashSpec {
    const NAME: &'static str = "bash";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "function.name" => NodeKind::Function,
        "const.name"    => NodeKind::Const,
    };
}
