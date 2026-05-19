//! Lua `LangSpec` — capture-name → NodeKind table.

use cgn_core::analyzer::lang_spec::LangSpec;
use cgn_core::graph::NodeKind;

pub struct LuaSpec;

impl LangSpec for LuaSpec {
    const NAME: &'static str = "lua";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "function.name" => NodeKind::Function,
        "struct.name"   => NodeKind::Class,
        "const.name"    => NodeKind::Const,
        "typedef.name"  => NodeKind::Typedef,
    };
}
