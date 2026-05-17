//! TypeScript `LangSpec` — capture-name → NodeKind table.
//!
//! Covers all symbol-producing captures in `queries.scm` + `frameworks.scm`.
//! Framework-detection captures (`express.*`, `nestjs.*`, `route.*`) are
//! metadata-only and are handled directly in `parser.rs`; they do NOT appear
//! here. Only captures that produce a standalone `RawNode` are listed.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct TypeScriptSpec;

impl LangSpec for TypeScriptSpec {
    const NAME: &'static str = "typescript";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "function.name"     => NodeKind::Function,
        "class.name"        => NodeKind::Class,
        "method.name"       => NodeKind::Method,
        "constructor.name"  => NodeKind::Constructor,
        "interface.name"    => NodeKind::Interface,
        "typedef.name"      => NodeKind::Typedef,
        "property.name"     => NodeKind::Property,
        "const.name"        => NodeKind::Const,
        "variable.name"     => NodeKind::Variable,
        "enum.name"         => NodeKind::Enum,
    };

    // TypeScript uses query-level scope anchoring for most kinds; no runtime
    // MODULE_SCOPED_CAPTURES gate needed. Default empty set inherited from trait.

    // Default BLOCK_BOUNDARY_TYPES from trait covers all relevant TS node kinds.
}
