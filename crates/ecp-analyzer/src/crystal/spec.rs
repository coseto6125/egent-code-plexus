//! Crystal `LangSpec` — capture-name → NodeKind table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct CrystalSpec;

impl LangSpec for CrystalSpec {
    const NAME: &'static str = "crystal";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"   => NodeKind::Class,
        "method.name"  => NodeKind::Method,
        "const.name"   => NodeKind::Const,
        "typedef.name" => NodeKind::Typedef,
    };
}
