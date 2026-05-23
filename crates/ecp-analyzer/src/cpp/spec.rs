//! C++ `LangSpec` — capture-name → NodeKind table.
//!
//! Metadata-only captures (`@heritage`, `@type`, `@export`, `@alias`,
//! `@import.source`, `@import`, `@field`, `@field.name`, `@var`, `@var.name`,
//! plus the root-span captures `@function`, `@class`, `@struct`, `@method`,
//! `@macro`, `@namespace`, `@enum_node`, `@typedef_node`) are NOT listed here —
//! they feed post-processing paths in `parser.rs`. Only the name-node captures
//! that drive the primary `kind` dispatch are listed.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct CppSpec;

impl LangSpec for CppSpec {
    const NAME: &'static str = "cpp";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "name.function"   => NodeKind::Function,
        "name.class"      => NodeKind::Class,
        "name.struct"     => NodeKind::Struct,
        "name.method"     => NodeKind::Method,
        "name.macro"      => NodeKind::Macro,
        "name.namespace"  => NodeKind::Namespace,
        "name.enum"       => NodeKind::Enum,
        "name.enumerator" => NodeKind::EnumVariant,
        "name.typedef"    => NodeKind::Typedef,
    };

    // C++ uses flat translation-unit / namespace scope; no runtime
    // module-scope gate needed for inline-class-member promotion —
    // that logic lives in `is_inline_class_member` in parser.rs.
    // (Default empty MODULE_SCOPED_CAPTURES inherited from trait.)

    // No BLOCK_BOUNDARY_TYPES override needed; defaults cover compound_statement.
}
