//! `cgn admin install-hook`: install reference-transaction hook in cwd's git common dir.
//! With `--claude-code`, instead installs entries into Claude Code's settings.json.

use crate::commands::admin::claude_code;
use crate::git::safe_exec;
use clap::Args;
use std::io::Write;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct InstallHookArgs {
    /// Force overwrite if a non-cgn hook already exists.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Skip hook chaining (don't preserve existing non-cgn hook).
    #[arg(long, default_value_t = false)]
    pub no_chain: bool,

    /// Install Claude Code settings.json hook entries instead of a git hook.
    #[arg(long, default_value_t = false)]
    pub claude_code: bool,

    /// CSV of events when --claude-code is set (session-start, user-prompt-submit,
    /// pre-tool-use, post-tool-use). Omit for an interactive multi-select.
    #[arg(long)]
    pub events: Option<String>,

    /// Override Claude Code settings.json path (default `~/.claude/settings.json`).
    #[arg(long, hide = true)]
    pub settings_path: Option<PathBuf>,
}

pub fn run(args: InstallHookArgs) -> Result<(), cgn_core::GnxError> {
    if args.claude_code {
        return claude_code::run_install_claude_code(
            args.events.as_deref(),
            args.settings_path.as_deref(),
        );
    }
    let out = safe_exec::git()
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .map_err(|e| cgn_core::GnxError::InvalidArgument(format!("git: {e}")))?;
    if !out.status.success() {
        return Err(cgn_core::GnxError::InvalidArgument(
            "not inside a git repository".into(),
        ));
    }
    let git_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let hook_dir = git_dir.join("hooks");
    std::fs::create_dir_all(&hook_dir)?;

    let hook_path = hook_dir.join("reference-transaction");
    let gnx_bin = std::env::current_exe()?;
    let gnx_bin_str = gnx_bin.to_string_lossy().into_owned();

    let existing_chain_target = if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path).unwrap_or_default();
        if existing.contains("cgn hook-handle") || existing.contains("hook-handle") {
            None
        } else if args.force || args.no_chain {
            let bak = hook_path.with_extension(format!("bak.{}", chrono::Utc::now().timestamp()));
            std::fs::rename(&hook_path, &bak)?;
            eprintln!("Existing hook backed up to {}", bak.display());
            None
        } else {
            let chained = hook_path.with_extension("chained-prev");
            std::fs::rename(&hook_path, &chained)?;
            Some(chained)
        }
    } else {
        None
    };

    let mut content = String::from("#!/bin/sh\n# cgn-managed reference-transaction hook\n");
    if let Some(prev) = &existing_chain_target {
        content.push_str(&format!("{} \"$@\" || exit $?\n", prev.display()));
    }
    content.push_str(&format!("exec \"{gnx_bin_str}\" hook-handle \"$@\"\n"));

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&hook_path)?;
    f.write_all(content.as_bytes())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    eprintln!(
        "Installed reference-transaction hook at {}",
        hook_path.display()
    );
    Ok(())
}
