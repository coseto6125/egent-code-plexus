//! Tests for safe_exec hardening wrapper (spec §8 H4).

use graph_nexus_cli::git::safe_exec;

#[test]
fn safe_exec_disables_protocol_ext() {
    let cmd = safe_exec::git();
    let args: Vec<String> = cmd.get_args().map(|s| s.to_string_lossy().into()).collect();
    assert!(
        args.iter().any(|a| a == "protocol.ext.allow=never"),
        "expected protocol.ext.allow=never in {args:?}"
    );
}

#[test]
fn safe_exec_disables_fsmonitor_editor_credential() {
    let cmd = safe_exec::git();
    let args: Vec<String> = cmd.get_args().map(|s| s.to_string_lossy().into()).collect();
    assert!(args.iter().any(|a| a == "core.fsmonitor="));
    assert!(args.iter().any(|a| a == "core.editor=false"));
    assert!(args.iter().any(|a| a == "credential.helper="));
}
