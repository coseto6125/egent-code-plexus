//! Per-language `FunctionMeta` extraction helpers.
//!
//! Each submodule exposes a single free function:
//!
//! ```rust,ignore
//! pub fn extract(
//!     root: tree_sitter::Node<'_>,
//!     source: &[u8],
//!     nodes: &[RawNode],
//!     file_category: FileCategory,
//! ) -> Vec<RawFunctionMeta>
//! ```
//!
//! Called at the end of each language parser's `parse_file`, after `nodes` is
//! finalized. Returns one `RawFunctionMeta` per Function/Method/Constructor
//! node (keyed by span). The builder converts these to `FunctionMeta` by
//! interning strings into the `StringPool`.

pub mod javascript;
pub mod python;
pub mod rust_lang;
pub mod typescript;
