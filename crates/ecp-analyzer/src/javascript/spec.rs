//! JavaScript `LangSpec` — capture-name → NodeKind table.
//!
//! Covers the symbol-producing captures dispatched through the spec
//! table. Route and framework captures (`route.*`, `express.*`,
//! `hapi.*`) are metadata-only and handled directly in `parser.rs`.
//!
//! ### `variable.name` is NOT in this table
//!
//! JS Variables have arrow-function dedup, `const`/`let`/`var` kind split,
//! and span-dedup against already-emitted Function nodes — `parser.rs`
//! dispatches `@variable.name` via a dedicated `idx_variable_name` path
//! ahead of the spec lookup. Listing it here would be misleading
//! (the entry would never be consulted), so it's intentionally omitted.

use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::graph::NodeKind;

pub struct JavaScriptSpec;

impl LangSpec for JavaScriptSpec {
    const NAME: &'static str = "javascript";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "name.function" => NodeKind::Function,
        "name.class"    => NodeKind::Class,
        "name.method"   => NodeKind::Method,
    };
}
