//! Shared menu-navigation helpers wrapping `dialoguer`.

use dialoguer::{theme::ColorfulTheme, Select};
use cgn_core::GnxError;

/// One menu entry: a short label and an optional one-line description.
///
/// `description` is `""` for items that don't need explanation (e.g.
/// `← Back`). When non-empty, the rendered string is
/// `"<label-padded>  (<description>)"`, with labels padded to a common
/// width so the description column lines up.
pub type Item<'a> = (&'a str, &'a str);

/// Show a [`Select`] prompt and return the chosen index.
///
/// Returns `None` when the user dismisses the prompt (e.g. EOF / Ctrl-C),
/// which callers should treat as "go back / exit".
pub fn select(
    theme: &ColorfulTheme,
    prompt: &str,
    items: &[Item<'_>],
) -> Result<Option<usize>, GnxError> {
    let rendered = render(items);
    let view: Vec<&str> = rendered.iter().map(String::as_str).collect();
    Select::with_theme(theme)
        .with_prompt(prompt)
        .items(&view)
        .default(0)
        .interact_opt()
        .map_err(|e| GnxError::Output(format!("dialoguer: {e}")))
}

pub(crate) fn render(items: &[Item<'_>]) -> Vec<String> {
    let max_label = items
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    items
        .iter()
        .map(|(label, desc)| format_item(label, desc, max_label))
        .collect()
}

fn format_item(label: &str, desc: &str, max_label: usize) -> String {
    if desc.is_empty() {
        return (*label).to_string();
    }
    let pad = max_label.saturating_sub(label.chars().count());
    format!("{label}{:pad$}  ({desc})", "", pad = pad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_pads_labels_to_longest_width() {
        let items: &[Item<'_>] = &[
            ("Indexes", "build, inspect, prune, drop"),
            ("Agent Integrations", "MCP / native / hooks"),
            ("Exit", ""),
        ];
        let rendered = render(items);
        assert_eq!(
            rendered,
            vec![
                "Indexes             (build, inspect, prune, drop)".to_string(),
                "Agent Integrations  (MCP / native / hooks)".to_string(),
                "Exit".to_string(),
            ]
        );
    }

    #[test]
    fn render_keeps_label_only_when_description_empty() {
        let items: &[Item<'_>] = &[("install", "write entry"), ("← Back", "")];
        let rendered = render(items);
        assert_eq!(rendered[1], "← Back");
        assert!(rendered[0].starts_with("install"));
        assert!(rendered[0].ends_with("(write entry)"));
    }
}
