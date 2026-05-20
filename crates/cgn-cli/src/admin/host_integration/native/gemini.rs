//! Gemini CLI native integration (Skill-based).

use crate::admin::host_integration::gemini_cli;
use crate::admin::status::HostStatus;
use cgn_core::CgnError;
use std::env;

const SKILL_NAME: &str = "cgn";

pub(crate) fn install_scripted() -> Result<(), CgnError> {
    let skill_path = find_skill_path().ok_or_else(|| {
        CgnError::Output(
            "could not locate cgn skill source: set CGN_GEMINI_SKILL_PATH or run from a checkout containing docs/skills/cgn"
                .into(),
        )
    })?;

    println!("Installing Gemini CLI native skill from {}...", skill_path);
    let output = gemini_cli::run(&["skills", "link", &skill_path])?;

    if output.status.success() {
        println!("✓ Gemini CLI native skill 'cgn' linked successfully.");
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(CgnError::Output(format!(
            "gemini skills link failed: {}",
            err.trim()
        )))
    }
}

pub(crate) fn uninstall_scripted() -> Result<(), CgnError> {
    let output = gemini_cli::run(&["skills", "uninstall", SKILL_NAME])?;
    if output.status.success() {
        println!("✓ Gemini CLI native skill 'cgn' uninstalled.");
    } else {
        println!("Gemini CLI native skill 'cgn' was not installed or already removed.");
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
    let list = String::from_utf8_lossy(&output.stdout);
    if list.contains(SKILL_NAME) {
        HostStatus::Installed {
            detail: "linked via gemini skills link".into(),
        }
    } else {
        HostStatus::Missing
    }
}

fn find_skill_path() -> Option<String> {
    if let Some(override_path) = env::var_os("CGN_GEMINI_SKILL_PATH") {
        let p = std::path::PathBuf::from(override_path);
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    let mut dir = env::current_dir().ok()?;
    loop {
        let path = dir.join("docs/skills/cgn");
        if path.exists() {
            return Some(path.to_string_lossy().into_owned());
        }
        if !dir.pop() {
            break;
        }
    }
    None
}
