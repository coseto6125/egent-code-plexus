//! `ecp dev` — internal parser-developer namespace. Hidden from top-level
//! `ecp --help`; not part of the LLM-facing CLI surface.
//!
//! Subcommands here surface metrics and audits useful for ecp parser
//! maintainers (uid hash-collision clusters, resolver-oracle diffs, etc.).
//! They are intentionally NOT exposed to end-users / agents because their
//! output is parser-implementation-shaped, not source-code-shaped.

use clap::Subcommand;
use ecp_core::EcpError;

pub mod uid_audit;

#[derive(Subcommand, Debug, Clone)]
pub enum DevCommands {
    /// Cluster-collapsed view of `uid-collision` BlindSpot records.
    /// Parser-maintainer audit only — NOT an LLM signal. For LLM-actionable
    /// blind spots use `ecp summary`.
    UidAudit(uid_audit::UidAuditArgs),
    /// Diff resolver dump against a language oracle (TP / FP / FN report).
    VerifyResolver(crate::commands::verify_resolver::VerifyResolverArgs),
}

pub fn run(cmd: DevCommands, cli_graph: &std::path::Path) -> Result<(), EcpError> {
    match cmd {
        DevCommands::UidAudit(args) => uid_audit::run(args, cli_graph),
        DevCommands::VerifyResolver(args) => crate::commands::verify_resolver::run(args),
    }
}
