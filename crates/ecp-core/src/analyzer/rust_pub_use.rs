//! The Rust `pub use` rows of one module, read from its text with the line
//! rules the index's module tree records them by. The index
//! (`ecp-analyzer`'s `rust::module_tree`) and the overlay's module trees
//! (`session::view`) both read a module's rows here, so the two sides see
//! the same re-exports by construction.

use rustc_hash::FxHashMap;
use std::sync::OnceLock;

/// One `pub use` re-export of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubUseEntry {
    /// The path prefix segments before the last `::` (e.g. `["inner"]` for
    /// `pub use inner::Widget`). May start with `crate`, `super`, `self`, or
    /// a crate name.
    pub path_prefix: Vec<String>,
    /// The original item name at the source (last segment before `as`),
    /// `*` for a glob re-export (`pub use path::*`).
    pub original_name: String,
    /// The module path of the file that contains this `pub use` statement.
    /// Used to resolve relative prefix segments (e.g. `nested` in
    /// `src/deep/mod.rs` means `deep::nested`, not root-level `nested`).
    pub containing_mod_path: Vec<String>,
}

/// A module's `pub use` rows, keyed by the name visible at the re-export
/// site: the alias after `as`, else the original name, `"*"` for a glob.
/// Of two rows with one key the later one stands.
pub type PubUseEntries = FxHashMap<String, PubUseEntry>;

fn line_comment_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"//[^\n]*").expect("line_comment_re"))
}

fn block_comment_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?s)/\*.*?\*/").expect("block_comment_re"))
}

/// Matches `pub use <path>::<Name>` and `pub use <path>::<Name> as <Alias>`.
/// Also matches `pub use <path>::*` for glob re-exports.
///
/// Capture groups:
///   1: module path prefix (everything before the last `::`)
///   2: exported item name (last segment, or `*` for glob)
///   3: optional alias after `as`
///
/// Visibility variants (`pub(crate)`, `pub(super)`, etc.) are all accepted
/// because all make the item reachable from *some* external scope.
fn pub_use_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?m)^\s*pub(?:\s*\([^)]*\))?\s+use\s+((?:[A-Za-z_][A-Za-z0-9_]*::)+)(\*|[A-Za-z_][A-Za-z0-9_]*)(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?\s*;"
        )
        .expect("pub_use_re")
    })
}

/// Matches the one-level group form `pub use <path>::{A, b::C, D as E};`.
/// Capture 1 is the shared prefix (with its trailing `::`), capture 2 the
/// brace body. A nested group inside the braces does not match, so that
/// statement keeps its pre-group behaviour (not collected).
fn pub_use_group_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?m)^\s*pub(?:\s*\([^)]*\))?\s+use\s+((?:[A-Za-z_][A-Za-z0-9_]*::)+)\{([^{};]*)\}\s*;"
        )
        .expect("pub_use_group_re")
    })
}

/// Strip `//` and `/* */` comments from source, preserving newlines so
/// MULTILINE regex anchors keep working.
pub fn strip_comments(src: &str) -> String {
    let s = block_comment_re().replace_all(src, |caps: &regex::Captures| {
        caps[0]
            .chars()
            .map(|c| if c == '\n' { '\n' } else { ' ' })
            .collect::<String>()
    });
    line_comment_re()
        .replace_all(&s, |caps: &regex::Captures| {
            caps[0]
                .chars()
                .map(|c| if c == '\n' { '\n' } else { ' ' })
                .collect::<String>()
        })
        .into_owned()
}

/// The `pub use` rows of the module whose raw text is `src` and whose
/// module path in its tree is `containing_mod_path` (e.g. `["deep"]` for
/// `src/deep/mod.rs`), stored in each entry so that relative path prefixes
/// (like `nested` in `pub use nested::W`) resolve as crate-rooted paths
/// during chain walking.
pub fn pub_use_entries(src: &str, containing_mod_path: &[String]) -> PubUseEntries {
    pub_use_entries_of_clean(&strip_comments(src), containing_mod_path)
}

/// [`pub_use_entries`] over text [`strip_comments`] already cleaned.
pub fn pub_use_entries_of_clean(clean: &str, containing_mod_path: &[String]) -> PubUseEntries {
    let mut entries = PubUseEntries::default();
    for cap in pub_use_re().captures_iter(clean) {
        // cap[1]: prefix like "inner::" or "crate::deep::" (trailing "::")
        // cap[2]: item name or "*"; cap[3]: optional alias
        insert_pub_use(
            cap[1].split("::"),
            &cap[2],
            cap.get(3).map(|m| m.as_str()),
            containing_mod_path,
            &mut entries,
        );
    }
    for cap in pub_use_group_re().captures_iter(clean) {
        for member in cap[2].split(',').map(str::trim).filter(|m| !m.is_empty()) {
            let (path, alias) = match member.split_once(" as ") {
                Some((path, alias)) => (path.trim(), Some(alias.trim())),
                None => (member, None),
            };
            let (sub, item) = match path.rsplit_once("::") {
                Some((sub, item)) => (Some(sub), item),
                None => (None, path),
            };
            if item == "self" {
                continue;
            }
            insert_pub_use(
                cap[1]
                    .split("::")
                    .chain(sub.into_iter().flat_map(|s| s.split("::"))),
                item,
                alias,
                containing_mod_path,
                &mut entries,
            );
        }
    }
    entries
}

/// Record one re-exported item: `pub use <prefix>::<item> [as <alias>]`.
fn insert_pub_use<'a>(
    prefix: impl Iterator<Item = &'a str>,
    item: &str,
    alias: Option<&str>,
    containing_mod_path: &[String],
    entries: &mut PubUseEntries,
) {
    let path_prefix: Vec<String> = prefix
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if path_prefix.is_empty() {
        return;
    }
    let exported_name = alias.unwrap_or(item).to_string();
    entries.insert(
        exported_name,
        PubUseEntry {
            path_prefix,
            original_name: item.to_string(),
            containing_mod_path: containing_mod_path.to_vec(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(prefix: &[&str], original: &str, containing: &[&str]) -> PubUseEntry {
        PubUseEntry {
            path_prefix: prefix.iter().map(|s| s.to_string()).collect(),
            original_name: original.to_string(),
            containing_mod_path: containing.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn entries_of(src: &str, containing: &[&str]) -> Vec<(String, PubUseEntry)> {
        let containing: Vec<String> = containing.iter().map(|s| s.to_string()).collect();
        let mut rows: Vec<_> = pub_use_entries(src, &containing).into_iter().collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    #[test]
    fn test_pub_use_entries_single_use_keys_item() {
        assert_eq!(
            entries_of("pub use a::b;\n", &["m"]),
            [("b".to_string(), entry(&["a"], "b", &["m"]))]
        );
    }

    #[test]
    fn test_pub_use_entries_alias_keys_alias_keeps_original() {
        assert_eq!(
            entries_of("pub use crate::deep::W as V;\n", &[]),
            [("V".to_string(), entry(&["crate", "deep"], "W", &[]))]
        );
    }

    #[test]
    fn test_pub_use_entries_glob_keys_star() {
        assert_eq!(
            entries_of("mod inner;\npub use inner::*;\n", &[]),
            [("*".to_string(), entry(&["inner"], "*", &[]))]
        );
    }

    #[test]
    fn test_pub_use_entries_group_with_self_skips_self() {
        assert_eq!(
            entries_of("pub use a::{self, b::C, D as E};\n", &["x", "y"]),
            [
                ("C".to_string(), entry(&["a", "b"], "C", &["x", "y"])),
                ("E".to_string(), entry(&["a"], "D", &["x", "y"])),
            ]
        );
    }

    #[test]
    fn test_pub_use_entries_restricted_visibility_is_collected() {
        assert_eq!(
            entries_of("pub(crate) use super::helper;\n", &["m"]),
            [("helper".to_string(), entry(&["super"], "helper", &["m"]))]
        );
    }

    #[test]
    fn test_pub_use_entries_commented_out_use_yields_nothing() {
        assert!(entries_of("// pub use x::y;\n/* pub use p::q; */\n", &[]).is_empty());
    }

    #[test]
    fn test_pub_use_entries_private_use_and_empty_text_yield_nothing() {
        assert!(entries_of("use a::b;\nuse super::*;\n", &[]).is_empty());
        assert!(entries_of("", &[]).is_empty());
    }

    #[test]
    fn test_pub_use_entries_later_row_of_one_name_stands() {
        assert_eq!(
            entries_of("pub use a::f;\npub use b::{f};\n", &[]),
            [("f".to_string(), entry(&["b"], "f", &[]))]
        );
    }
}
