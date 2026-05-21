//! Table-driven schema-field extraction infrastructure.
//!
//! Mirrors ref-gitnexus `field-extractors/generic.ts` (192 L): instead of N
//! separate hardcoded detectors, every ORM / schema framework ships a
//! `SchemaFieldConfig` constant and `extract_schema_fields` dispatches all of
//! them uniformly.
//!
//! T4-1 (this PR): trait + config struct + dispatch loop.
//! T4-2..T4-6: concrete `SchemaFieldConfig` constants for Pydantic,
//! SQLAlchemy, TypeScript interfaces, protobuf, and OpenAPI.

pub mod config;
pub mod extract;

pub use config::SchemaFieldConfig;
pub use extract::{classify_python_type, classify_ts_type, extract_schema_fields};
