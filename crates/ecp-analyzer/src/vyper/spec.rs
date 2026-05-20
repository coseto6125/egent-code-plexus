//! Vyper `LangSpec` — capture-name → NodeKind table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct VyperSpec;

impl LangSpec for VyperSpec {
    const NAME: &'static str = "vyper";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "function.name"  => NodeKind::Function,
        "const.name"     => NodeKind::Const,
        "typedef.name"   => NodeKind::Typedef,
    };
}
