use clap::Args;
use crate::engine::Engine;
use graph_nexus_core::GnxError;

#[derive(Args, Debug, Clone)]
pub struct ContractsArgs {
    /// Contract kind: routes / queue / rpc / all
    #[arg(long, default_value = "all")]
    pub kind: String,
    /// Only show contracts without a paired consumer/producer
    #[arg(long, default_value_t = false)]
    pub unmatched_only: bool,
    /// Repo selector
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(_args: ContractsArgs, _engine: &Engine) -> Result<(), GnxError> {
    Err(GnxError::Output("contracts command stub — implement in Task 3.2".into()))
}
