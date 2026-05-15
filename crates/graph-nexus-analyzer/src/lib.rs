// graph-nexus-analyzer

/// SHA256 fingerprint of every parser.rs / queries.scm / shared helper
/// file at build time, set by `build.rs`. Re-exported here so downstream
/// crates can pin cache invalidation to "anything in the parser layer
/// changed" without depending on the build.rs env var (which is scoped
/// to *this* crate's compilation only — `env!()` resolves in the caller's
/// crate context, not the dep's).
pub const PARSER_FINGERPRINT: &str = env!("GRAPH_NEXUS_PARSER_FINGERPRINT");

pub mod bash;
pub mod c;
pub mod c_sharp;
pub mod cairo;
pub mod calls;
pub mod cpp;
pub mod crystal;
pub mod dart;
pub mod docker_compose;
pub mod dockerfile;
pub mod embeddings;
pub mod entry_points;
pub mod fetch_shape;
pub mod framework_confidence;
pub mod framework_helpers;
pub mod github_actions;
pub mod go;
pub mod hcl;
pub mod identifier_finder;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod lua;
pub mod markdown;
pub mod move_lang;
pub mod nim;
pub mod php;
pub mod python;
pub mod resolution;
pub mod route_detector;
pub mod ruby;
pub mod rust;
pub mod solidity;
pub mod sql;
pub mod swift;
pub mod typescript;
pub mod verilog;
pub mod vyper;
pub mod yaml;
pub mod zig;
