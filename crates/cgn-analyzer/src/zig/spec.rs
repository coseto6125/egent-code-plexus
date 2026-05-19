//! Zig `LangSpec` — capture-name → NodeKind table.

use cgn_core::analyzer::lang_spec::LangSpec;
use cgn_core::graph::NodeKind;

pub struct ZigSpec;

impl LangSpec for ZigSpec {
    const NAME: &'static str = "zig";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "function.name" => NodeKind::Function,
        "struct.name"   => NodeKind::Class,
        "const.name"    => NodeKind::Const,
    };
}
