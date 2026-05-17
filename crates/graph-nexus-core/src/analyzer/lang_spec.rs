//! `LangSpec`: centralized, table-driven per-language parsing contract.
//!
//! Every `LanguageProvider::parse_file` reduces to:
//!   1. parse source with tree-sitter
//!   2. run the language's symbol query
//!   3. for each capture, look up its semantic `NodeKind` in a phf table
//!   4. apply per-language post-processing (Function→Method in class body, ...)
//!   5. emit `RawNode` into the resulting `LocalGraph`
//!
//! Step 3's mapping table is the single source of truth for "what counts
//! as a symbol in language X". Moving it out of `parser.rs` match arms
//! into a `phf::Map<&'static str, NodeKind>` makes the contract:
//!   - auditable (one file lists every kind decision)
//!   - benchmarkable (lookups inline via `phf`'s perfect hash, zero alloc)
//!   - extensible without touching parser.rs (new lang = new `spec.rs`)
//!
//! This module is intentionally minimal — it does NOT subsume framework
//! detection, call extraction, or import resolution. Those stay in
//! `parser.rs` because they need lang-specific AST walking that no
//! generic table can capture. `LangSpec` is the symbol-kind backbone.
//!
//! ## Hot-path performance
//!
//! Providers pre-resolve `CAPTURE_KIND` into a
//! `Vec<Option<NodeKind>>` indexed by tree-sitter capture index at
//! construction time (see `kotlin/parser.rs::KotlinProvider::new` for
//! the reference). The hot parse loop then dispatches by integer
//! index — identical machine code to the previous hard-coded
//! if/else chain (~1-5 ns per capture). The const `phf::Map` itself
//! is only consulted once per provider lifetime, not per node, so
//! its perfect-hash cost (~3-5 ns) is amortised to zero.

use crate::graph::NodeKind;

/// The capture-name → NodeKind contract for one language. Implementors
/// supply a `phf::Map` indexed by the tree-sitter query capture name
/// (e.g., `"class.name"`, `"function.name"`); the framework consumes
/// it during symbol extraction in `parser.rs`.
///
/// `LangSpec` is intentionally split from `LanguageProvider` (which
/// remains the outer entry point owning tree-sitter parsing, framework
/// detection, and call extraction). A provider's `parse_file` is free
/// to consult `Self::KIND_MAP` for the "what kind is this capture"
/// decision while keeping all other logic in plain Rust.
pub trait LangSpec {
    /// The language's display name. Must match `LanguageProvider::name()`.
    const NAME: &'static str;

    /// Maps tree-sitter capture name (left side of `@foo` in queries.scm)
    /// to the `NodeKind` we emit. Capture names that yield `None` are
    /// metadata-only (e.g., `@export`, `@heritage`) and don't produce
    /// a standalone `RawNode`.
    const CAPTURE_KIND: phf::Map<&'static str, NodeKind>;

    /// Capture names that resolve to a NodeKind only when the captured
    /// AST node sits at module scope. The default `should_emit_at_scope`
    /// gates these against `BLOCK_BOUNDARY_TYPES` ancestor walk.
    ///
    /// Empty by default — opt-in per language for kinds whose grammar
    /// node type is shared between module-scope (emit) and block-scope
    /// (drop), like Kotlin `property_declaration` or Java
    /// `local_variable_declaration`.
    const MODULE_SCOPED_CAPTURES: phf::Set<&'static str> = phf::phf_set! {};

    /// Tree-sitter node kinds that act as block-scope boundaries (we
    /// walk ancestors; if we hit one of these BEFORE reaching a
    /// module-scope container, the capture is block-scoped).
    ///
    /// A sensible default covers most C-family + Python languages.
    /// Override per language when grammar uses non-standard names
    /// (e.g., Kotlin `function_body`, Swift `function_body`).
    const BLOCK_BOUNDARY_TYPES: phf::Set<&'static str> = phf::phf_set! {
        "block",
        "compound_statement",
        "function_body",
        "function_definition",
        "function_declaration",
        "method_declaration",
        "method_definition",
        "arrow_function",
        "function_expression",
        "lambda",
        "lambda_literal",
        "constructor_body",
    };

    /// Tree-sitter node kinds that act as module-scope containers (top
    /// of file). Walking up from a capture and hitting one of these
    /// means we're at module scope.
    const MODULE_SCOPE_TYPES: phf::Set<&'static str> = phf::phf_set! {
        "program",
        "source_file",
        "module",
        "translation_unit",
        "compilation_unit",
    };

    /// Default kind classifier for one capture. Looks up `CAPTURE_KIND`;
    /// when the capture is in `MODULE_SCOPED_CAPTURES`, walks the AST
    /// upward and drops the emission if a block boundary is reached
    /// before a module-scope container.
    ///
    /// Per-language `parser.rs` may override this entirely by ignoring
    /// the helper and implementing its own dispatch — but the default
    /// covers ~80% of cases for free.
    #[inline]
    fn classify_capture(capture_name: &str, capture_node: tree_sitter::Node) -> Option<NodeKind> {
        let kind = *Self::CAPTURE_KIND.get(capture_name)?;
        if Self::MODULE_SCOPED_CAPTURES.contains(capture_name)
            && !is_at_module_scope::<Self>(capture_node)
        {
            return None;
        }
        Some(kind)
    }
}

/// Walk `node`'s ancestors. Returns `true` iff a `MODULE_SCOPE_TYPES`
/// ancestor is reached before any `BLOCK_BOUNDARY_TYPES`. The capture
/// node itself does not count (loop starts at `node.parent()`).
#[inline]
pub fn is_at_module_scope<L: LangSpec + ?Sized>(node: tree_sitter::Node) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        let k = n.kind();
        if L::BLOCK_BOUNDARY_TYPES.contains(k) {
            return false;
        }
        if L::MODULE_SCOPE_TYPES.contains(k) {
            return true;
        }
        cur = n.parent();
    }
    // Reached the root without crossing either set — treat as module.
    true
}
