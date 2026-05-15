use clap::Args;
use crate::engine::Engine;
use graph_nexus_core::GnxError;

#[derive(Args, Debug, Clone)]
pub struct ScanArgs {
    /// File path to scan for symbol references
    pub file: String,
    /// Also flag uncertain references
    #[arg(long, default_value_t = false)]
    pub strict: bool,
    /// Repository selector
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(_args: ScanArgs, _engine: &Engine) -> Result<(), GnxError> {
    Err(GnxError::Output("scan command stub — implement in Task 3.1".into()))
}
