//! Go `LangSpec` — capture-name → NodeKind table.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct GoSpec;

impl LangSpec for GoSpec {
    const NAME: &'static str = "go";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "struct.name"    => NodeKind::Struct,
        "interface.name" => NodeKind::Interface,
        "method.name"    => NodeKind::Method,
        "function.name"  => NodeKind::Function,
    };
}
