//! HCL `LangSpec` — capture-name → NodeKind table.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct HclSpec;

impl LangSpec for HclSpec {
    const NAME: &'static str = "hcl";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"   => NodeKind::Class,
        "const.name"   => NodeKind::Const,
        "output.name"  => NodeKind::Const,
        "typedef.name" => NodeKind::Typedef,
    };
}
