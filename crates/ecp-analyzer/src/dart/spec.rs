//! Dart `LangSpec` — capture-name → NodeKind table.
//!
//! Capture names that produce special AST-driven dispatch (`@property.name`
//! and `@var.name` paths in `parser.rs`) are absent here — those emit through
//! dedicated branches that need the root span node. Metadata-only captures
//! (`@heritage`, `@type`, `@import.*`, `@decorator`, `@var.*`) are also
//! absent. Everything that maps 1-to-1 from a `.name` capture to a NodeKind
//! lives in this table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct DartSpec;

impl LangSpec for DartSpec {
    const NAME: &'static str = "dart";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"       => NodeKind::Class,
        "function.name"    => NodeKind::Function,
        "method.name"      => NodeKind::Method,
        "constructor.name" => NodeKind::Constructor,
        "typedef.name"     => NodeKind::Typedef,
        "interface.name"   => NodeKind::Interface,
        "trait.name"       => NodeKind::Trait,
        "property.name"    => NodeKind::Property,
        "enum.name"        => NodeKind::Enum,
        "annotation.name"  => NodeKind::Annotation,
    };

    // Dart grammar scope-anchors via query patterns; no MODULE_SCOPED_CAPTURES
    // runtime gate needed.

    const BLOCK_BOUNDARY_TYPES: phf::Set<&'static str> = phf::phf_set! {
        "function_body",
        "block",
        "function_expression_body",
        "lambda_expression",
        "constructor_body",
        "method_declaration",
        "function_declaration",
    };
}
