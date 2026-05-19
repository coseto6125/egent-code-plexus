//! C# `LangSpec` — capture-name → NodeKind table.

use cgn_core::analyzer::lang_spec::LangSpec;
use cgn_core::graph::NodeKind;

pub struct CSharpSpec;

impl LangSpec for CSharpSpec {
    const NAME: &'static str = "c_sharp";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "name.class"       => NodeKind::Class,
        "name.method"      => NodeKind::Method,
        "name.interface"   => NodeKind::Interface,
        "name.function"    => NodeKind::Function,
        "property.name"    => NodeKind::Property,
        "variable.name"    => NodeKind::Variable,
        "constructor.name" => NodeKind::Constructor,
        "namespace.name"   => NodeKind::Namespace,
        "enum.name"        => NodeKind::Enum,
        "struct.name"      => NodeKind::Struct,
    };
}
