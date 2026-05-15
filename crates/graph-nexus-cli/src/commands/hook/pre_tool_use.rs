//! PreToolUse handler — stub. Implemented in T7 (depends on T1).

use super::common::HookInput;
use graph_nexus_core::GnxError;

pub fn handle(_input: &HookInput) -> Result<(), GnxError> {
    Ok(())
}
