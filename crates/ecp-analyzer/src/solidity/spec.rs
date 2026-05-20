//! Solidity `LangSpec` — capture-name → NodeKind table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct SoliditySpec;

impl LangSpec for SoliditySpec {
    const NAME: &'static str = "solidity";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"     => NodeKind::Class,
        "method.name"    => NodeKind::Method,
        "function.name"  => NodeKind::Function,
        "const.name"     => NodeKind::Const,
        "state_var.name" => NodeKind::Const,
        "typedef.name"   => NodeKind::Typedef,
    };
}
