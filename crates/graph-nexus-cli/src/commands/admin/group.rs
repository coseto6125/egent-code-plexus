use clap::Subcommand;
use graph_nexus_core::GnxError;

#[derive(Subcommand, Debug)]
pub enum GroupCommands {
    /// Add a repo to a group (auto-creates group)
    Add {
        repo: String,
        group: String,
    },
    /// Remove a repo from a group (auto-deletes empty group)
    Remove {
        repo: String,
        group: String,
    },
}

pub fn run(_cmd: GroupCommands) -> Result<(), GnxError> {
    Err(GnxError::Output("admin group commands stub — implement in Task 4.2".into()))
}
