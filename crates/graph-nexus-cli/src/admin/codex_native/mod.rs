#![allow(dead_code)] // Public API consumed by host-integration generator (test was removed pending MCP re-wiring).

//! Codex CLI native fork integration.
//!
//! Generates install artifacts that the user applies to their `openai/codex`
//! fork. The generator auto-discovers gnx tools via `inventory::iter` (works
//! because `graph-nexus-cli` already links the 8 registered commands) and
//! emits two files:
//!
//! - `~/.config/gnx/host-integration/codex-cli/install.sh` — shell script
//!   that patches `codex-rs/core/Cargo.toml`, copies `gnx.rs`, and registers
//!   the module in `tools/mod.rs`.
//! - `~/.config/gnx/host-integration/codex-cli/gnx.rs` — one `impl Tool for`
//!   block per registered gnx command (best-effort scaffold, user may need to
//!   adapt to the actual codex-rs `Tool` trait shape).
//!
//! # Usage
//! ```no_run
//! use graph_nexus_cli::admin::codex_native;
//! use std::path::Path;
//!
//! let home_gnx = Path::new("/home/user/.config/gnx");
//! let patch = codex_native::generate_patch(home_gnx).unwrap();
//! codex_native::install_instructions(&patch);
//! ```

mod patch_template;

use graph_nexus_core::GnxError;
use patch_template::{build_gnx_rs, build_install_sh, collect_gnx_tools, count_tools_in_file};
use std::path::{Path, PathBuf};

/// Metadata returned by [`generate_patch`].
pub struct GeneratedPatch {
    /// Directory containing the generated files:
    /// `<home_gnx>/host-integration/codex-cli/`
    pub path: PathBuf,
    /// Number of gnx tools embedded in the generated `gnx.rs`.
    pub tool_count: usize,
    /// Total bytes written across both generated files.
    pub bytes_written: usize,
}

/// Generate the codex-cli integration files and write them to
/// `<home_gnx>/host-integration/codex-cli/`.
///
/// Two files are written:
/// - `install.sh` — shell script for patching the codex-rs fork.
/// - `gnx.rs`    — Tool impl stubs, one per registered gnx command.
///
/// Returns patch metadata on success.
///
/// # Arguments
/// - `home_gnx`: base config directory (e.g. `~/.config/gnx`). Override in
///   tests to avoid writing to the real config directory.
pub fn generate_patch(home_gnx: &Path) -> Result<GeneratedPatch, GnxError> {
    let tools = collect_gnx_tools();
    let tool_count = tools.len();

    let install_sh = build_install_sh(&tools);
    let gnx_rs = build_gnx_rs(&tools)?;

    let out_dir = home_gnx.join("host-integration").join("codex-cli");
    std::fs::create_dir_all(&out_dir)?;

    let install_sh_path = out_dir.join("install.sh");
    let gnx_rs_path = out_dir.join("gnx.rs");

    std::fs::write(&install_sh_path, &install_sh)?;
    std::fs::write(&gnx_rs_path, &gnx_rs)?;

    // Make the install script executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&install_sh_path)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(&install_sh_path, perms)?;
    }

    let bytes_written = install_sh.len() + gnx_rs.len();

    Ok(GeneratedPatch {
        path: out_dir,
        tool_count,
        bytes_written,
    })
}

/// Print step-by-step install instructions for the user.
pub fn install_instructions(patch: &GeneratedPatch) {
    println!("Files written to {}", patch.path.display());
    println!();
    println!("To install in your openai/codex fork:");
    println!("  cd <your codex fork root>");
    println!("  sh {}/install.sh", patch.path.display());
    println!("  cargo build -p codex-core");
    println!();
    println!("Embedded {} gnx tools.", patch.tool_count);
    println!("After build, your codex-cli binary will register gnx_* tools natively (zero IPC).");
    println!();
    println!("NOTE: the generated gnx.rs is a best-effort scaffold.");
    println!("If `cargo build` fails, adjust the `impl Tool for` blocks to match");
    println!("codex-rs's current Tool trait signature.");
}

/// Status of the gnx integration in a codex-rs checkout.
#[derive(Debug)]
pub enum Status {
    /// `codex-rs/core/src/tools/gnx.rs` does not exist.
    Missing,
    /// The marker line was found; `tool_count` is the number of `impl Tool for`
    /// blocks detected in the file.
    Installed { tool_count: usize },
    /// The file exists but the marker is absent — likely an older or unrelated
    /// version.
    Outdated { reason: String },
}

/// Probe whether the gnx integration is installed in `codex_repo` by
/// checking for the marker line `// gnx-integration-marker-v1` inside
/// `codex-rs/core/src/tools/gnx.rs`.
pub fn status(codex_repo: &Path) -> Status {
    let marker_path = codex_repo.join("codex-rs/core/src/tools/gnx.rs");
    if !marker_path.exists() {
        return Status::Missing;
    }
    let content = std::fs::read_to_string(&marker_path).unwrap_or_default();
    if content.contains("gnx-integration-marker-v1") {
        Status::Installed {
            tool_count: count_tools_in_file(&content),
        }
    } else {
        Status::Outdated {
            reason: "marker not found".to_string(),
        }
    }
}

/// Print uninstall instructions. We do not auto-revert the user's git tree.
pub fn uninstall_instructions(codex_repo: &Path) {
    println!("To uninstall gnx native integration:");
    println!("  cd {}", codex_repo.display());
    println!("  git checkout codex-rs/core/  # discards the gnx-integrated files");
}
