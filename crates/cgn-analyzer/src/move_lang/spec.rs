//! Move `LangSpec` — capture-name → NodeKind table.

use cgn_core::analyzer::lang_spec::LangSpec;
use cgn_core::graph::NodeKind;

pub struct MoveSpec;

impl LangSpec for MoveSpec {
    const NAME: &'static str = "move";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"    => NodeKind::Class,
        "function.name" => NodeKind::Function,
        "struct.name"   => NodeKind::Class,
        "const.name"    => NodeKind::Const,
        "typedef.name"  => NodeKind::Typedef,
    };
}
