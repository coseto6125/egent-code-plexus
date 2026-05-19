//! Config workflows for `cgn admin`.

use crate::admin::menu::{self, select};
use crate::commands::admin::config as config_cmd;
use dialoguer::{theme::ColorfulTheme, Input};
use cgn_core::config::{config_path, load};
use cgn_core::CgnError;
use std::path::PathBuf;

const MENU: &[menu::Item<'_>] = &[
    ("View config", "print the parsed cgn.toml as TOML"),
    ("Edit config", "open cgn.toml in $EDITOR"),
    ("Validate config", "load + check cgn.toml without writing"),
    ("← Back", ""),
];

pub fn run(theme: &ColorfulTheme) -> Result<(), CgnError> {
    loop {
        let choice = select(theme, "Config", MENU)?;
        match choice {
            Some(0) => view_config(theme)?,
            Some(1) => edit_config(theme)?,
            Some(2) => validate_config(theme)?,
            Some(3) | None => return Ok(()),
            _ => unreachable!(),
        }
    }
}

fn view_config(theme: &ColorfulTheme) -> Result<(), CgnError> {
    let repo = input_path(theme)?;
    let cfg = load(&repo).map_err(CgnError::InvalidArgument)?;
    let body = toml::to_string_pretty(&cfg)
        .map_err(|e| CgnError::Serialization(format!("config TOML: {e}")))?;
    println!("Config: {}", config_path(&repo).display());
    print!("{body}");
    Ok(())
}

fn edit_config(theme: &ColorfulTheme) -> Result<(), CgnError> {
    let repo = input_path(theme)?;
    config_cmd::run(config_cmd::ConfigArgs {
        repo: Some(repo.to_string_lossy().into_owned()),
    })
}

fn validate_config(theme: &ColorfulTheme) -> Result<(), CgnError> {
    let repo = input_path(theme)?;
    load(&repo).map_err(CgnError::InvalidArgument)?;
    println!("✓ Config is valid: {}", config_path(&repo).display());
    Ok(())
}

fn input_path(theme: &ColorfulTheme) -> Result<PathBuf, CgnError> {
    Input::with_theme(theme)
        .with_prompt("Repo path")
        .default(".".to_string())
        .interact_text()
        .map(PathBuf::from)
        .map_err(|e| CgnError::Output(format!("dialoguer: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_menu_matches_target_order() {
        let labels: Vec<&str> = MENU.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            vec!["View config", "Edit config", "Validate config", "← Back"]
        );
    }
}
