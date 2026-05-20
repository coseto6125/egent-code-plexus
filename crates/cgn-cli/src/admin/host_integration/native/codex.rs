//! Codex CLI native integration (Rust workspace dep, zero-IPC path).

use crate::admin::status::HostStatus;
use cgn_core::CgnError;
use dialoguer::theme::ColorfulTheme;
use std::fs;
use std::path::{Path, PathBuf};

const MARKER: &str = "cgn-codex-native-integration-v1";
const PATCH_NAME: &str = "codex-cli.patch";

pub fn install(_theme: &ColorfulTheme) {
    match run_install() {
        Ok(path) => {
            println!("Codex CLI native patch written to {}", path.display());
            println!("Apply it in your openai/codex fork, then wire the generated tool into Codex's tool registry.");
        }
        Err(e) => eprintln!("Codex CLI native install failed: {e}"),
    }
}

pub fn uninstall(_theme: &ColorfulTheme) {
    match run_uninstall() {
        Ok(path) => println!("Codex CLI native patch removed from {}", path.display()),
        Err(e) => eprintln!("Codex CLI native uninstall failed: {e}"),
    }
}

pub fn status() -> HostStatus {
    if let Some(checkout) = std::env::var_os("CGN_CODEX_CLI_CHECKOUT").map(PathBuf::from) {
        return status_from_checkout(&checkout);
    }
    let patch = patch_path();
    if patch.exists() {
        HostStatus::Outdated {
            reason: format!(
                "patch prepared at {}; set CGN_CODEX_CLI_CHECKOUT to verify the fork",
                patch.display()
            ),
        }
    } else {
        HostStatus::Missing
    }
}

pub(crate) fn run_install() -> Result<PathBuf, CgnError> {
    let path = patch_path();
    let cgn_root =
        std::env::current_dir().map_err(|e| CgnError::Output(format!("current_dir: {e}")))?;
    write_patch(&path, &cgn_root)?;
    Ok(path)
}

pub(crate) fn run_uninstall() -> Result<PathBuf, CgnError> {
    let path = patch_path();
    remove_patch(&path)?;
    Ok(path)
}

fn remove_patch(path: &Path) -> Result<(), CgnError> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn patch_path() -> PathBuf {
    config_root()
        .join("cgn")
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

fn write_patch(path: &Path, cgn_root: &Path) -> Result<(), CgnError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let root = cgn_root.to_string_lossy();
    let body = format!(
        r#"diff --git a/codex-rs/core/src/tools/cgn.rs b/codex-rs/core/src/tools/cgn.rs
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/codex-rs/core/src/tools/cgn.rs
@@ -0,0 +1,48 @@
+// {MARKER}
+//
+// Native code-graph-nexus integration scaffold.
+//
+// Add these dependencies to codex-rs/core/Cargo.toml:
+//
+// code-graph-nexus = {{ path = "{root}/crates/cgn-cli" }}
+// cgn-core = {{ path = "{root}/crates/cgn-core" }}
+//
+// Then register the tool(s) from this module in Codex's tool registry.
+// The exact registry file changes across Codex releases, so this patch
+// intentionally adds the stable integration module and leaves the final
+// registration hunk to the fork maintainer.
+
+use cgn_core::CgnError;
+
+pub const CGN_NATIVE_MARKER: &str = "{MARKER}";
+
+pub fn cgn_command_args(tool: &str, raw_args: &[String]) -> Result<Vec<String>, CgnError> {{
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

fn checkout_contains_marker(path: &Path) -> Result<bool, CgnError> {
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
    fn write_patch_includes_marker_and_cgn_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let patch = dir.path().join(PATCH_NAME);
        write_patch(&patch, Path::new("/repo/code-graph-nexus")).expect("write patch");

        let body = fs::read_to_string(patch).expect("read patch");
        assert!(body.contains(MARKER));
        assert!(body.contains("/repo/code-graph-nexus/crates/cgn-cli"));
    }

    #[test]
    fn status_from_checkout_detects_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("cgn.rs");
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
        write_patch(&patch, Path::new("/repo/code-graph-nexus")).expect("write patch");

        assert!(matches!(
            status_from_checkout(dir.path()),
            HostStatus::Missing
        ));
    }

    #[test]
    fn remove_patch_deletes_existing_patch_and_missing_is_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let patch = dir.path().join(PATCH_NAME);
        write_patch(&patch, Path::new("/repo/code-graph-nexus")).expect("write patch");

        remove_patch(&patch).expect("remove patch");
        remove_patch(&patch).expect("remove missing patch");

        assert!(!patch.exists());
    }
}
