pub mod algorithms;
pub mod analyzer;
pub mod config;
pub mod cypher;
pub mod daemon;
pub mod error;
pub mod graph;
pub mod graph_query;
pub mod peer;
pub mod pool;
pub mod registry;
pub mod session;
pub mod uid;

pub use error::{EcpError, EcpResult};

/// Confidence threshold for `--high-trust-only` filtering on impact / detect-changes.
/// Edges below this confidence (e.g. framework-aware refs like FastAPI `Depends()` at 0.6)
/// are excluded from traversal when the flag is enabled.
pub const HIGH_TRUST_CONFIDENCE: f32 = 0.8;
