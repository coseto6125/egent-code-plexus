//! C `LangSpec` — capture-name → NodeKind table.
//!
//! Metadata-only captures (`@type`, `@import.source`, `@field`, `@field.name`,
//! `@var`, `@var.name`, plus the root-span captures `@function`, `@struct`,
//! `@union`, `@enum`, `@typedef`, `@macro`) are NOT listed here — they feed
//! post-processing paths in `parser.rs` and don't produce a standalone
//! `RawNode` via the spec table. Only the name-node captures that set the
//! primary `kind` are listed.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct CSpec;

impl LangSpec for CSpec {
    const NAME: &'static str = "c";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "function.name" => NodeKind::Function,
        "struct.name"   => NodeKind::Struct,
        "union.name"    => NodeKind::Struct,
        "enum.name"     => NodeKind::Enum,
        "typedef.name"  => NodeKind::Typedef,
        "macro.name"    => NodeKind::Macro,
    };

    // C uses flat translation-unit scope; no runtime module-scope gate needed.
    // (Default empty MODULE_SCOPED_CAPTURES inherited from trait.)

    // C compound_statement is the block boundary; the defaults cover it.
    // No override needed.
}
