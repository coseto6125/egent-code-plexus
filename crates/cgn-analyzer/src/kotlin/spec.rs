//! Kotlin `LangSpec` — capture-name → NodeKind table.
//!
//! This is the reference impl. Pattern mirrored across all 21 langs in
//! Phase B / C of the langspec rollout.
//!
//! ### Scope discipline
//!
//! Kotlin's `queries.scm` already scope-anchors `@variable` to
//! `(source_file (property_declaration ...))` and `@property` to
//! `(class_body (property_declaration ...))`, so the runtime
//! `MODULE_SCOPED_CAPTURES` gate is NOT needed for Kotlin —
//! over-emission is prevented at query time, not at classification
//! time. The const stays empty here. Other languages whose grammars
//! don't allow query-level scope anchors (e.g., Java) will populate it.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct KotlinSpec;

impl LangSpec for KotlinSpec {
    const NAME: &'static str = "kotlin";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class.name"       => NodeKind::Class,
        "constructor.name" => NodeKind::Constructor,
        "function.name"    => NodeKind::Function,
        "property.name"    => NodeKind::Property,
        "variable.name"    => NodeKind::Variable,
        // `enum class X { A, B, C }` entries — tree-sitter-kotlin produces
        // an `enum_entry` per identifier inside `enum_class_body`. The
        // parent enum class is already captured as `class.name` (promoted
        // to `Enum` by `is_enum_class` at parser.rs:282); the entries
        // themselves were silently dropped pre-fix, leaving 15 ref_over
        // rows on `.sample_repo` (OperatingSystem.{Linux,MacOS,Windows,...}
        // family in `Dart/extensions/intellij/.../*.kt`).
        "enum_entry.name"  => NodeKind::Enum,
    };

    // Kotlin uses query-level scope anchoring; no runtime scope gate needed.
    // (Default empty MODULE_SCOPED_CAPTURES inherited from trait.)

    // Block boundary set: include Kotlin-specific names beyond the default.
    const BLOCK_BOUNDARY_TYPES: phf::Set<&'static str> = phf::phf_set! {
        "function_body",
        "function_declaration",
        "lambda_literal",
        "anonymous_function",
        "block",
        "constructor_body",
        "secondary_constructor",
    };
}
