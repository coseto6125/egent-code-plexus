//! Codex CLI native integration (Rust workspace dep, zero-IPC path).

use crate::admin::status::HostStatus;
use dialoguer::theme::ColorfulTheme;
use ecp_core::EcpError;
use std::fs;
use std::path::{Path, PathBuf};

const MARKER: &str = "ecp-codex-native-integration-v1";
const PATCH_NAME: &str = "codex-cli.patch";
const PENDING_NATIVE_TOOLS: &str =
    "TODO: pending Codex tool-registry wiring; native-tools install is not enabled yet";

pub fn install(_theme: &ColorfulTheme) {
    println!("Codex CLI native-tools: {PENDING_NATIVE_TOOLS}");
}

pub fn uninstall(_theme: &ColorfulTheme) {
    match run_uninstall() {
        Ok(path) => println!("Codex CLI native patch removed from {}", path.display()),
        Err(e) => eprintln!("Codex CLI native uninstall failed: {e}"),
    }
}

pub fn status() -> HostStatus {
    if let Some(checkout) = std::env::var_os("ECP_CODEX_CLI_CHECKOUT").map(PathBuf::from) {
        return status_from_checkout(&checkout);
    }
    let patch = patch_path();
    if patch.exists() {
        HostStatus::Outdated {
            reason: format!(
                "stale experimental patch at {}; {PENDING_NATIVE_TOOLS}",
                patch.display()
            ),
        }
    } else {
        HostStatus::Missing
    }
}

#[allow(dead_code)]
pub(crate) fn run_install() -> Result<PathBuf, EcpError> {
    // TODO(native-tools): generate a checkout-aware patch that includes Codex's
    // dependency and tool-registry hunks. The adapter-only patch looked
    // installable but could not actually register tools in Codex, so keep this
    // command disabled until registry wiring is implemented.
    Err(EcpError::InvalidArgument(PENDING_NATIVE_TOOLS.into()))
}

pub(crate) fn pending_message() -> &'static str {
    PENDING_NATIVE_TOOLS
}

pub(crate) fn run_uninstall() -> Result<PathBuf, EcpError> {
    let path = patch_path();
    remove_patch(&path)?;
    prune_empty_parents(&path);
    Ok(path)
}

fn remove_patch(path: &Path) -> Result<(), EcpError> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Walk up from `<config>/ecp/host-integration/<file>` removing each ancestor
/// that is now empty. `remove_dir` fails on non-empty dirs, so this cannot
/// delete unrelated files. Stops the moment we leave the `ecp/` segment so
/// we never touch `~/.config/` or anything above ours.
fn prune_empty_parents(start: &Path) {
    let mut cursor = start.parent();
    while let Some(dir) = cursor {
        let is_ours = dir
            .components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new("ecp"));
        if !is_ours {
            break;
        }
        if fs::remove_dir(dir).is_err() {
            break;
        }
        cursor = dir.parent();
    }
}

fn patch_path() -> PathBuf {
    config_root()
        .join("ecp")
        .join("host-integration")
        .join(PATCH_NAME)
}

fn config_root() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config")
}

// TODO(native-tools): revive this once `run_install` can also write the Codex
// Cargo.toml and tool-registry hunks for a concrete checkout.
#[allow(dead_code)]
fn write_patch(path: &Path, ecp_root: &Path) -> Result<(), EcpError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let root = ecp_root.to_string_lossy();
    let body = format!(
        r#"diff --git a/codex-rs/core/src/tools/ecp.rs b/codex-rs/core/src/tools/ecp.rs
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/codex-rs/core/src/tools/ecp.rs
@@ -0,0 +1,44 @@
+// {MARKER}
+//
+// Native egent-code-plexus integration adapter.
+//
+// Add these dependencies to codex-rs/core/Cargo.toml:
+//
+// egent-code-plexus = {{ path = "{root}/crates/ecp-cli" }}
+// ecp-core = {{ path = "{root}/crates/ecp-core" }}
+// serde_json = "1"
+//
+// Register these helpers in Codex's tool registry. The registry file changes
+// across Codex releases, so this patch intentionally adds the stable adapter
+// module and leaves the final registry hunk to the fork maintainer.
+
+use std::path::Path;
+
+use ecp_cli::native::{{self, NativeTool, ToolResult}};
+use ecp_core::EcpError;
+use serde_json::Value;
+
+pub const ECP_NATIVE_MARKER: &str = "{MARKER}";
+
+pub fn ecp_tools() -> Vec<NativeTool> {{
+    native::tools()
+}}
+
+pub fn ecp_tool_argv(name: &str, args: Value) -> Result<Vec<String>, EcpError> {{
+    native::tool_argv(name, args)
+}}
+
+pub fn ecp_call_spawn(binary: &Path, name: &str, args: Value) -> Result<ToolResult, EcpError> {{
+    native::call_spawn(binary, name, args)
+}}
+
+pub fn ecp_command_args(tool: &str, raw_args: &[String]) -> Result<Vec<String>, EcpError> {{
+    let mut argv = Vec::with_capacity(raw_args.len() + 1);
+    argv.push(tool.to_string());
+    argv.extend(raw_args.iter().cloned());
+    Ok(argv)
+}}
"#
    );
    let tmp = path.with_extension("patch.tmp");
    fs::write(&tmp, body)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn status_from_checkout(checkout: &Path) -> HostStatus {
    if !checkout.exists() {
        return HostStatus::Missing;
    }
    match checkout_contains_marker(checkout) {
        Ok(true) => HostStatus::Installed {
            detail: format!("fork marker found under {}", checkout.display()),
        },
        Ok(false) => HostStatus::Missing,
        Err(e) => HostStatus::Outdated {
            reason: format!("cannot inspect {}: {e}", checkout.display()),
        },
    }
}

fn checkout_contains_marker(path: &Path) -> Result<bool, EcpError> {
    if path.is_file() {
        let raw = fs::read_to_string(path)?;
        return Ok(raw.contains(MARKER));
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, ".git" | "target" | "node_modules"))
        {
            continue;
        }
        if child.is_dir() {
            if checkout_contains_marker(&child)? {
                return Ok(true);
            }
        } else if child.extension().and_then(|ext| ext.to_str()) == Some("rs")
            && checkout_contains_marker(&child)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_patch_includes_marker_and_ecp_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let patch = dir.path().join(PATCH_NAME);
        write_patch(&patch, Path::new("/repo/egent-code-plexus")).expect("write patch");

        let body = fs::read_to_string(patch).expect("read patch");
        assert!(body.contains(MARKER));
        assert!(body.contains("/repo/egent-code-plexus/crates/ecp-cli"));
    }

    #[test]
    fn status_from_checkout_detects_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("ecp.rs");
        fs::write(&file, format!("const MARKER: &str = \"{MARKER}\";")).expect("write marker");

        assert!(matches!(
            status_from_checkout(dir.path()),
            HostStatus::Installed { .. }
        ));
    }

    #[test]
    fn prepared_patch_is_not_reported_as_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let patch = dir.path().join(PATCH_NAME);
        write_patch(&patch, Path::new("/repo/egent-code-plexus")).expect("write patch");

        assert!(matches!(
            status_from_checkout(dir.path()),
            HostStatus::Missing
        ));
    }

    #[test]
    fn remove_patch_deletes_existing_patch_and_missing_is_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let patch = dir.path().join(PATCH_NAME);
        write_patch(&patch, Path::new("/repo/egent-code-plexus")).expect("write patch");

        remove_patch(&patch).expect("remove patch");
        remove_patch(&patch).expect("remove missing patch");

        assert!(!patch.exists());
    }

    #[test]
    fn prune_empty_parents_collapses_empty_ancestors_up_to_ecp() {
        // Mirror the on-disk layout `<config>/ecp/host-integration/<patch>`
        // and confirm both `host-integration/` and `ecp/` disappear once
        // the patch is gone — but the synthetic `<config>` root stays.
        let dir = tempfile::tempdir().expect("tempdir");
        let host_integ = dir.path().join("ecp").join("host-integration");
        fs::create_dir_all(&host_integ).unwrap();
        let patch = host_integ.join(PATCH_NAME);
        fs::write(&patch, "patch body").unwrap();

        fs::remove_file(&patch).unwrap();
        prune_empty_parents(&patch);

        assert!(!host_integ.exists(), "host-integration/ should be pruned");
        assert!(
            !dir.path().join("ecp").exists(),
            "ecp/ should be pruned too"
        );
        assert!(
            dir.path().exists(),
            "the synthetic config root must NOT be touched"
        );
    }

    #[test]
    fn prune_empty_parents_keeps_non_empty_dirs() {
        // If the user (or a sibling tool) wrote something into ecp/, we
        // must not delete it.
        let dir = tempfile::tempdir().expect("tempdir");
        let host_integ = dir.path().join("ecp").join("host-integration");
        fs::create_dir_all(&host_integ).unwrap();
        let patch = host_integ.join(PATCH_NAME);
        let sibling = dir.path().join("ecp").join("user-marker");
        fs::write(&patch, "patch body").unwrap();
        fs::write(&sibling, "user data").unwrap();

        fs::remove_file(&patch).unwrap();
        prune_empty_parents(&patch);

        assert!(
            !host_integ.exists(),
            "host-integration/ (now empty) should be pruned"
        );
        assert!(
            dir.path().join("ecp").exists(),
            "ecp/ must stay because user-marker lives in it"
        );
        assert!(sibling.exists(), "sibling file untouched");
    }
}
