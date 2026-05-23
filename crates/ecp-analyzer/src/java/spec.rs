//! Java `LangSpec` — capture-name → NodeKind table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct JavaSpec;

impl LangSpec for JavaSpec {
    const NAME: &'static str = "java";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"          => NodeKind::Class,
        "interface.name"      => NodeKind::Interface,
        "method.name"         => NodeKind::Method,
        "constructor.name"    => NodeKind::Constructor,
        "property.name"       => NodeKind::Property,
        "variable.name"       => NodeKind::Variable,
        "enum.name"           => NodeKind::Enum,
        "enum_constant.name"  => NodeKind::EnumVariant,
        "annotation.name"     => NodeKind::Annotation,
    };
}
