//! Cross-language post-process passes that run AFTER all per-language
//! parsers have emitted their `LocalGraph` and BEFORE the builder finalises
//! the CSR offset arrays.
//!
//! These passes mirror upstream gitnexus's `reconcileOwnership` pipeline
//! step (see `_source_code/ARCHITECTURE.md` L258) — language-neutral
//! derivations that fix gaps no single parser is responsible for filling.

pub mod class_membership;
pub mod event_topic_mirrors;
pub mod imports_edges;
pub mod overrides;
pub mod schema_field_mirrors;
