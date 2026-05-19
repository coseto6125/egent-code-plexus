//! Ruby `LangSpec` — capture-name → NodeKind table.
//!
//! `queries.scm` captures that drive NodeKind dispatch:
//!   `class`  — the `(class ...)` root node → `Class`
//!   `module` — the `(module ...)` root node → `Trait`
//!             (modules are mixin targets; `Trait` matches ref-gitnexus semantics)
//!   `method` — `(method ...)` and `(singleton_method ...)` root nodes → `Method`
//!   `const`  — `(assignment left: (constant) ...)` → `Const`
//!
//! All other captures (`name`, `heritage`, `import.name`, `decorator`,
//! `route.*`, `attr_args`, `mixin_module`, `alias.*`, `delegator_*`, etc.)
//! are metadata-only and absent from this map — their handling lives in
//! `parser.rs` because it requires imperative AST walking or cross-match
//! state that no generic table can capture.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct RubySpec;

impl LangSpec for RubySpec {
    const NAME: &'static str = "ruby";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "class"  => NodeKind::Class,
        "module" => NodeKind::Trait,
        "method" => NodeKind::Method,
        "const"  => NodeKind::Const,
    };
}
