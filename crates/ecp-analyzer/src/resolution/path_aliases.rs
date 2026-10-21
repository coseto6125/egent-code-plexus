//! The alias table lives in `ecp-core` so the session overlay reads the same
//! type; this path keeps the analyzer's existing imports working.

pub use ecp_core::analyzer::path_aliases::*;
