//! Rust `LangSpec` — capture-name → NodeKind table.
//!
//! Covers every `*.name` capture in `queries.scm` that produces a named
//! symbol node. Root-span captures (`@struct`, `@function`, `@method`, …)
//! don't carry a NodeKind entry here — they only set `root_span_node` in
//! the parser hot loop; `kind` comes from the paired `.name` capture lookup.
//!
//! ### Class / Method / Interface mapping
//!
//! Rust has no class or interface concept, so:
//! - `struct_item.name` → `Struct` (not `Class`); post-processing in
//!   `pipeline.rs` bridges `struct ↔ impl` for class-membership.
//! - `function_item.name` → `Function` (parser demotes to `Method` when the
//!   root node is inside an `impl_item` body — see `parser.rs` dedup logic).
//! - `trait_item.name` → `Trait` (not `Interface`); the graph layer maps
//!   `Trait` where needed for polyglot consumers.

use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::graph::NodeKind;

pub struct RustSpec;

impl LangSpec for RustSpec {
    const NAME: &'static str = "rust";

    const CAPTURE_KIND: phf::Map<&'static str, NodeKind> = phf::phf_map! {
        "struct_item.name"     => NodeKind::Struct,
        "enum_item.name"       => NodeKind::Enum,
        "trait_item.name"      => NodeKind::Trait,
        "function_item.name"   => NodeKind::Function,
        "module_item.name"     => NodeKind::Module,
        "type_alias_item.name" => NodeKind::Typedef,
        "const_item.name"      => NodeKind::Const,
        "impl_item.name"       => NodeKind::Impl,
        "macro_item.name"      => NodeKind::Macro,
        "property.name"        => NodeKind::Property,
    };
}
