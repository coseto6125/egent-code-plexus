//! Shared menu-navigation helpers wrapping `dialoguer`.

use dialoguer::{theme::ColorfulTheme, Select};
use graph_nexus_core::GnxError;

/// Show a [`Select`] prompt and return the chosen index.
///
/// Returns `None` when the user dismisses the prompt (e.g. EOF / Ctrl-C),
/// which callers should treat as "go back / exit".
pub fn select(
    theme: &ColorfulTheme,
    prompt: &str,
    items: &[&str],
) -> Result<Option<usize>, GnxError> {
    Select::with_theme(theme)
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact_opt()
        .map_err(|e| GnxError::Output(format!("dialoguer: {e}")))
}
