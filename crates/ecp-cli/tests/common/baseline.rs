//! Known-difference baselines (architecture plan, area 11). A test reduces
//! its finding to sorted lines, each line prefixed by its section; the
//! baseline file must hold exactly those lines. A new line fails the test,
//! and so does a baseline line that no longer occurs: the lane that fixed it
//! deletes the line in the same change. Shared through `#[path]`.
//!
//! File shape: the first line is `# owner: <lane that removes this file>`,
//! then the difference lines. Other `#` lines and blank lines are ignored.
//!
//! `ECP_BLESS=1` writes the observed lines instead of failing. An existing
//! owner line is kept; a new file gets the caller's default owner. An empty
//! observation removes the file, because no difference needs no owner.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const OWNER_PREFIX: &str = "# owner: ";

/// `ECP_BLESS` is set to a value other than `0` or empty.
pub fn bless_mode() -> bool {
    std::env::var("ECP_BLESS").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// The baseline's owner line value and its difference lines. A missing file
/// reads as no owner and no lines.
pub fn read(path: &Path) -> (Option<String>, BTreeSet<String>) {
    let Ok(text) = fs::read_to_string(path) else {
        return (None, BTreeSet::new());
    };
    let owner = text
        .lines()
        .next()
        .and_then(|first| first.strip_prefix(OWNER_PREFIX))
        .map(|owner| owner.trim().to_string());
    let lines = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    (owner, lines)
}

/// The baseline text for `observed`: the owner line, then the sorted lines.
pub fn render(owner: &str, observed: &BTreeSet<String>) -> String {
    let mut text = format!("{OWNER_PREFIX}{owner}\n");
    for line in observed {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// The section of a difference line: its first whitespace-separated word.
fn section(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// Per section, the observed lines missing from the baseline (`new`) and the
/// baseline lines no longer observed (`stale`). Empty when they match.
pub fn mismatches(observed: &BTreeSet<String>, expected: &BTreeSet<String>) -> Vec<String> {
    let sections: BTreeSet<&str> = observed
        .iter()
        .chain(expected)
        .map(|l| section(l))
        .collect();
    let mut report = Vec::new();
    for name in sections {
        let new: Vec<&String> = observed
            .iter()
            .filter(|l| section(l) == name && !expected.contains(*l))
            .collect();
        let stale: Vec<&String> = expected
            .iter()
            .filter(|l| section(l) == name && !observed.contains(*l))
            .collect();
        if new.is_empty() && stale.is_empty() {
            continue;
        }
        report.push(format!(
            "section `{name}`: {} new, {} stale",
            new.len(),
            stale.len()
        ));
        report.extend(new.iter().map(|l| format!("  + {l}")));
        report.extend(stale.iter().map(|l| format!("  - {l}")));
    }
    report
}

/// Compare `observed` with the baseline at `path`, or rewrite the baseline
/// under bless mode. Panics with every section's new and stale lines.
pub fn check(path: &Path, default_owner: &str, observed: &BTreeSet<String>) {
    let (owner, expected) = read(path);
    if path.exists() {
        assert!(
            owner.is_some(),
            "{} must start with `{OWNER_PREFIX}<lane>`",
            path.display()
        );
    }
    if bless_mode() {
        if observed.is_empty() {
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
            return;
        }
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let owner = owner.unwrap_or_else(|| default_owner.to_string());
        fs::write(path, render(&owner, observed)).unwrap();
        return;
    }
    let report = mismatches(observed, &expected);
    assert!(
        report.is_empty(),
        "{} does not match the observed differences (`+` new, `-` no longer \
         differs). Fix the defect, or rerun with ECP_BLESS=1 and review the \
         rewritten baseline.\n{}",
        path.display(),
        report.join("\n")
    );
}
