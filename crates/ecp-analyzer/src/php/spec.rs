//! PHP `LangSpec` — capture-name → NodeKind table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct PhpSpec;

impl LangSpec for PhpSpec {
    const NAME: &'static str = "php";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "name.function"  => NodeKind::Function,
        "name.class"     => NodeKind::Class,
        "name.interface" => NodeKind::Interface,
        "name.method"    => NodeKind::Method,
        "name.property"  => NodeKind::Property,
        "name.namespace" => NodeKind::Namespace,
        "name.trait"     => NodeKind::Trait,
        "name.enum"      => NodeKind::Enum,
        "name.const"     => NodeKind::Const,
    };
}
