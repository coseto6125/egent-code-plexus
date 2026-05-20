//! Cairo `LangSpec` — capture-name → NodeKind table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct CairoSpec;

impl LangSpec for CairoSpec {
    const NAME: &'static str = "cairo";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "function.name" => NodeKind::Function,
        "struct.name"   => NodeKind::Class,
        "class.name"    => NodeKind::Class,
        "const.name"    => NodeKind::Const,
        "typedef.name"  => NodeKind::Typedef,
    };
}
