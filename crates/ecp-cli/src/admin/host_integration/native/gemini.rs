//! Gemini CLI native integration (Skill-based).

use crate::admin::host_integration::gemini_cli;
use crate::admin::status::HostStatus;
use ecp_core::EcpError;
use std::env;

const SKILL_NAME: &str = "ecp";

pub(crate) fn install_scripted() -> Result<(), EcpError> {
    let skill_path = find_skill_path().ok_or_else(|| {
        EcpError::Output(
            "could not locate ecp skill source: set ECP_GEMINI_SKILL_PATH or run from a checkout containing docs/skills/ecp"
                .into(),
        )
    })?;

    println!("Installing Gemini CLI native skill from {}...", skill_path);
    let output = gemini_cli::run(&["skills", "link", "--consent", &skill_path])?;

    if output.status.success() {
        println!("✓ Gemini CLI native skill 'ecp' linked successfully.");
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(EcpError::Output(format!(
            "gemini skills link failed: {}",
            err.trim()
        )))
    }
}

pub(crate) fn uninstall_scripted() -> Result<(), EcpError> {
    let output = gemini_cli::run(&["skills", "uninstall", SKILL_NAME])?;
    if output.status.success() {
        println!("✓ Gemini CLI native skill 'ecp' uninstalled.");
    } else {
        println!("Gemini CLI native skill 'ecp' was not installed or already removed.");
    }
    Ok(())
}

pub fn status() -> HostStatus {
    let Ok(output) = gemini_cli::run(&["skills", "list"]) else {
        return HostStatus::Missing;
    };
    if !output.status.success() {
        return HostStatus::Missing;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let pattern = format!("{} [", SKILL_NAME);
    if stdout.contains(&pattern) || stderr.contains(&pattern) {
        HostStatus::Installed {
            detail: "linked via gemini skills link".into(),
        }
    } else {
        HostStatus::Missing
    }
}

fn find_skill_path() -> Option<String> {
    if let Some(override_path) = env::var_os("ECP_GEMINI_SKILL_PATH") {
        let p = std::path::PathBuf::from(override_path);
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    let mut dir = env::current_dir().ok()?;
    loop {
        let path = dir.join("docs/skills/ecp");
        if path.exists() {
            return Some(path.to_string_lossy().into_owned());
        }
        if !dir.pop() {
            break;
        }
    }
    None
}
