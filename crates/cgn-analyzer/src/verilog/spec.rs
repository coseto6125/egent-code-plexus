//! Verilog `LangSpec` — capture-name → NodeKind table.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct VerilogSpec;

impl LangSpec for VerilogSpec {
    const NAME: &'static str = "verilog";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"       => NodeKind::Class,
        "method.name"      => NodeKind::Method,
        "const.name"       => NodeKind::Const,
        "class_prop.name"  => NodeKind::Property,
        "typedef.name"     => NodeKind::Typedef,
    };
}
