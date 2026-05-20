//! Swift `LangSpec` — capture-name → NodeKind table.
//!
//! Capture names that produce special AST-driven dispatch in `parser.rs`
//! (`class.name` → Class/Struct/Enum via `swift_decl_keyword`, `function.name`
//! → Function/Method via `is_class_method`) still live here with their
//! *default* kind; the parser post-processes the result. Metadata-only
//! captures (`@export`, `@heritage`, `@type`, `@import.*`, `@constructor`,
//! `@typealias`, `@property*`) are absent — they never produce a standalone
//! RawNode via the spec table.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct SwiftSpec;

impl LangSpec for SwiftSpec {
    const NAME: &'static str = "swift";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"    => NodeKind::Class,
        "function.name" => NodeKind::Function,
        "method.name"   => NodeKind::Method,
        "interface.name"=> NodeKind::Interface,
        "trait.name"    => NodeKind::Trait,
    };

    // Swift grammar already scope-anchors captures at query level; no runtime
    // MODULE_SCOPED_CAPTURES gate needed.

    const BLOCK_BOUNDARY_TYPES: phf::Set<&'static str> = phf::phf_set! {
        "function_body",
        "computed_property",
        "willset_didset_block",
        "lambda_literal",
        "block",
        "if_statement",
        "guard_statement",
        "for_statement",
        "while_statement",
    };
}
