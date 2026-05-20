//! SQL `LangSpec` — capture-name → NodeKind table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct SqlSpec;

impl LangSpec for SqlSpec {
    const NAME: &'static str = "sql";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"    => NodeKind::Class,
        "function.name" => NodeKind::Function,
        "const.name"    => NodeKind::Const,
        "typedef.name"  => NodeKind::Typedef,
    };
}
