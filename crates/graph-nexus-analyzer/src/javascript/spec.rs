//! JavaScript `LangSpec` — capture-name → NodeKind table.
//!
//! Covers the symbol-producing captures in `queries.scm`.
//! Route and framework captures (`route.*`, `express.*`, `hapi.*`) are
//! metadata-only and handled directly in `parser.rs`.
//!
//! ### Variable handling
//!
//! JS Variables (`@variable.name`) have special post-processing: arrow-function
//! dedup, `const`/`let`/`var` kind split, and span dedup against already-emitted
//! Function nodes. That logic stays in `parser.rs`; `variable.name` is listed
//! here so the spec table is the complete source of truth for all symbol captures.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct JavaScriptSpec;

impl LangSpec for JavaScriptSpec {
    const NAME: &'static str = "javascript";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "name.function" => NodeKind::Function,
        "name.class"    => NodeKind::Class,
        "name.method"   => NodeKind::Method,
        "variable.name" => NodeKind::Variable,
    };

    // JavaScript query patterns are not scope-anchored at query time for
    // Variables, so runtime dedup logic in parser.rs handles that path.
    // MODULE_SCOPED_CAPTURES gate is not used (default empty set from trait).
}
