//! `ecp dev` — internal parser-developer namespace. Hidden from top-level
//! `ecp --help`; not part of the LLM-facing CLI surface.
//!
//! Subcommands here surface metrics and audits useful for ecp parser
//! maintainers (uid hash-collision clusters, resolver-oracle diffs, etc.).
//! They are intentionally NOT exposed to end-users / agents because their
//! output is parser-implementation-shaped, not source-code-shaped.

use clap::Subcommand;
use ecp_core::EcpError;

pub mod pr_analyze;
pub mod uid_audit;

#[derive(Subcommand, Debug, Clone)]
pub enum DevCommands {
    /// Classify a PR by graph-aware area/risk/cross-PR conflict; emit JSON
    /// for the ecp-pr-analyze workflow to apply labels + status.
    PrAnalyze(pr_analyze::PrAnalyzeArgs),
    /// Cluster-collapsed view of `uid-collision` BlindSpot records.
    /// Parser-maintainer audit only — NOT an LLM signal. For LLM-actionable
    /// blind spots use `ecp summary`.
    UidAudit(uid_audit::UidAuditArgs),
    /// Diff resolver dump against a language oracle (TP / FP / FN report).
    VerifyResolver(crate::commands::verify_resolver::VerifyResolverArgs),
}

pub fn run(cmd: DevCommands, cli_graph: &std::path::Path) -> Result<(), EcpError> {
    match cmd {
        DevCommands::PrAnalyze(args) => pr_analyze::run(args, cli_graph),
        DevCommands::UidAudit(args) => uid_audit::run(args, cli_graph),
        DevCommands::VerifyResolver(args) => crate::commands::verify_resolver::run(args),
    }
}
