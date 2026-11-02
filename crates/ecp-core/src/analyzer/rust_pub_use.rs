//! The Rust `pub use` rows of one module, read from its text with the line
//! rules the index's module tree records them by, and the `mod`
//! declarations and `#[path]` attributes the tree is walked by. The index
//! (`ecp-analyzer`'s `rust::module_tree`) and the overlay's module trees
//! (`session::view`) both read a module's text here, so the two sides see
//! the same re-exports by construction.

use crate::analyzer::ladder::{Published, ReExport};
use crate::analyzer::rust_paths::rust_module_dir;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
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
/// Single-item rows are read before group rows; of two rows with one key,
/// the one read last stands.
pub type PubUseEntries = FxHashMap<String, PubUseEntry>;

/// A module's text with its `//` and `/* */` comments blanked
/// ([`strip_comments`]): the only text the line rules here read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stripped(String);

impl Stripped {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn line_comment_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"//[^\n]*").expect("line_comment_re"))
}

fn block_comment_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?s)/\*.*?\*/").expect("block_comment_re"))
}

fn mod_decl_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
            .expect("mod_decl_re")
    })
}

fn path_attr_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#"#\[path\s*=\s*"([^"]+)"\]"#).expect("path_attr_re"))
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

/// `src` with its `//` and `/* */` comments blanked, newlines kept so
/// MULTILINE regex anchors keep working.
pub fn strip_comments(src: &str) -> Stripped {
    fn blank(caps: &regex::Captures) -> String {
        caps[0]
            .chars()
            .map(|c| if c == '\n' { '\n' } else { ' ' })
            .collect()
    }
    let s = block_comment_re().replace_all(src, blank);
    Stripped(line_comment_re().replace_all(&s, blank).into_owned())
}

/// The names of the file-backed `mod NAME;` declarations of `text`, in
/// text order.
pub fn mod_decls(text: &Stripped) -> impl Iterator<Item = &str> {
    mod_decl_re()
        .captures_iter(text.as_str())
        .filter_map(|cap| cap.get(1).map(|name| name.as_str()))
}

/// The `#[path = "rel"]` attributes of `text` that stand within the three
/// lines before a `mod NAME;`: `NAME → rel`.
pub fn path_attrs(text: &Stripped) -> FxHashMap<String, String> {
    let mut out: FxHashMap<String, String> = FxHashMap::default();
    let lines: Vec<&str> = text.as_str().lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(caps) = path_attr_re().captures(line) {
            let rel_path = caps[1].to_string();
            let lookahead_end = (i + 4).min(lines.len());
            for next_line in &lines[(i + 1)..lookahead_end] {
                if let Some(mcap) = mod_decl_re().captures(next_line) {
                    out.insert(mcap[1].to_string(), rel_path.clone());
                    break;
                }
            }
        }
    }
    out
}

/// The steps the index's module tree walk takes from the Rust file `file`
/// whose text is `text`: one `(NAME, child file)` per file-backed
/// `mod NAME;` ([`mod_decls`]), in text order. A `#[path]` module
/// ([`path_attrs`]) steps to its path relative to `file`'s directory,
/// whether or not that file exists. Any other steps to `NAME.rs`, else
/// `NAME/mod.rs`, the first that `exists` holds, beside `file` when it is
/// the tree's root (`root`) whatever its file name (`[lib] path =
/// "src/core.rs"`), and in its module directory ([`rust_module_dir`])
/// otherwise; with neither file it takes no step.
pub fn mod_steps<'t>(
    text: &'t Stripped,
    file: &Path,
    root: bool,
    exists: impl Fn(&Path) -> bool,
) -> Vec<(&'t str, PathBuf)> {
    let attrs = path_attrs(text);
    let dir = if root {
        file.parent().map(Path::to_path_buf)
    } else {
        rust_module_dir(file)
    };
    mod_decls(text)
        .filter_map(|name| {
            let child = match attrs.get(name) {
                Some(rel) => file
                    .parent()
                    .map_or_else(|| PathBuf::from(rel), |dir| dir.join(rel)),
                None => {
                    let dir = dir.as_deref()?;
                    let flat = dir.join(format!("{name}.rs"));
                    if exists(&flat) {
                        flat
                    } else {
                        let nested = dir.join(name).join("mod.rs");
                        exists(&nested).then_some(nested)?
                    }
                }
            };
            Some((name, child))
        })
        .collect()
}

/// Whether `text` declares a module with a `#[path]` attribute, by the rule
/// [`path_attrs`] reads them with.
pub fn declares_path_attr(text: &Stripped) -> bool {
    path_attr_re().is_match(text.as_str()) && !path_attrs(text).is_empty()
}

/// The `pub use` rows of the module whose text is `text` and whose module
/// path in its tree is `containing_mod_path` (e.g. `["deep"]` for
/// `src/deep/mod.rs`), stored in each entry so that relative path prefixes
/// (like `nested` in `pub use nested::W`) resolve as crate-rooted paths
/// during chain walking.
pub fn pub_use_entries(text: &Stripped, containing_mod_path: &[String]) -> PubUseEntries {
    let text = text.as_str();
    let mut entries = PubUseEntries::default();
    for cap in pub_use_re().captures_iter(text) {
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
    for cap in pub_use_group_re().captures_iter(text) {
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

/// What a module publishes `name` through, given its `pub use` row by key
/// (`entry`): the row naming `name`, else its glob row, else its own item.
pub fn published<'e>(entry: impl Fn(&str) -> Option<&'e PubUseEntry>, name: &str) -> Published<'e> {
    match entry(name).or_else(|| entry("*")) {
        Some(entry) => Published::ReExport(ReExport {
            prefix: &entry.path_prefix,
            original: &entry.original_name,
            containing: &entry.containing_mod_path,
        }),
        None => Published::Own,
    }
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
        let mut rows: Vec<_> = pub_use_entries(&strip_comments(src), &containing)
            .into_iter()
            .collect();
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
    fn test_pub_use_entries_single_then_group_row_of_one_name_group_stands() {
        assert_eq!(
            entries_of("pub use a::f;\npub use b::{f};\n", &[]),
            [("f".to_string(), entry(&["b"], "f", &[]))]
        );
    }

    /// Group rows are read after single-item rows whatever the text order,
    /// so the group row stands though the single row comes later.
    #[test]
    fn test_pub_use_entries_group_then_single_row_of_one_name_group_stands() {
        assert_eq!(
            entries_of("pub use b::{f};\npub use a::f;\n", &[]),
            [("f".to_string(), entry(&["b"], "f", &[]))]
        );
    }

    #[test]
    fn test_strip_comments_blanks_comments_keeps_newlines_and_length() {
        let src = "a /* x\ny */ b // c\nd";
        let stripped = strip_comments(src);
        assert_eq!(stripped.as_str(), "a     \n     b     \nd");
        assert_eq!(stripped.as_str().len(), src.len());
    }

    #[test]
    fn test_mod_decls_file_backed_mods_in_text_order() {
        let text =
            strip_comments("pub mod b;\nmod a;\n// mod c;\nmod inline { }\npub(crate) mod d;\n");
        assert_eq!(mod_decls(&text).collect::<Vec<_>>(), ["b", "a", "d"]);
    }

    #[test]
    fn test_path_attrs_attribute_before_mod_maps_mod_to_path() {
        let text = strip_comments("#[path = \"actual.rs\"]\npub mod facade;\nmod right;\n");
        assert_eq!(
            path_attrs(&text).into_iter().collect::<Vec<_>>(),
            [("facade".to_string(), "actual.rs".to_string())]
        );
    }

    #[test]
    fn test_path_attrs_commented_out_or_absent_attribute_yields_nothing() {
        assert!(path_attrs(&strip_comments("// #[path = \"x.rs\"]\nmod m;\n")).is_empty());
        assert!(path_attrs(&strip_comments("mod m;\n")).is_empty());
        assert!(path_attrs(&strip_comments("")).is_empty());
    }

    #[test]
    fn test_declares_path_attr_attribute_before_mod_true_absent_or_commented_false() {
        assert!(declares_path_attr(&strip_comments(
            "mod a;\n#[path = \"real.rs\"]\npub mod inner;\n"
        )));
        assert!(!declares_path_attr(&strip_comments(
            "// #[path = \"x.rs\"]\nmod m;\n"
        )));
        assert!(!declares_path_attr(&strip_comments(
            "mod m;\npub use m::f;\n"
        )));
        assert!(!declares_path_attr(&strip_comments("")));
        assert!(
            !declares_path_attr(&strip_comments("#[path = \"x.rs\"]\nfn f() {}\n")),
            "an attribute on no `mod` relocates nothing"
        );
    }

    fn steps_of(src: &str, file: &str, root: bool, files: &[&str]) -> Vec<(String, PathBuf)> {
        mod_steps(&strip_comments(src), Path::new(file), root, |path| {
            files.iter().any(|f| Path::new(f) == path)
        })
        .into_iter()
        .map(|(name, child)| (name.to_string(), child))
        .collect()
    }

    #[test]
    fn test_mod_steps_root_flat_then_mod_rs_beside_root_missing_file_takes_no_step() {
        assert_eq!(
            steps_of(
                "mod a;\npub mod b;\nmod gone;\nmod inline { }\n",
                "src/core.rs",
                true,
                &["src/a.rs", "src/a/mod.rs", "src/b/mod.rs", "src/core/a.rs"],
            ),
            [
                ("a".to_string(), PathBuf::from("src/a.rs")),
                ("b".to_string(), PathBuf::from("src/b/mod.rs")),
            ]
        );
    }

    #[test]
    fn test_mod_steps_cargo_bin_root_child_finds_sibling_module() {
        assert_eq!(
            steps_of(
                "mod support;\n",
                "src/bin/tool.rs",
                false,
                &["src/bin/support.rs", "src/bin/tool/support.rs"],
            ),
            [("support".to_string(), PathBuf::from("src/bin/support.rs"))]
        );
    }

    #[test]
    fn test_mod_steps_ordinary_file_finds_stem_directory_child() {
        assert_eq!(
            steps_of(
                "mod c;\n",
                "src/a/b.rs",
                false,
                &["src/a/b/c.rs", "src/a/c.rs"]
            ),
            [("c".to_string(), PathBuf::from("src/a/b/c.rs"))]
        );
    }

    #[test]
    fn test_mod_steps_path_attr_relative_to_file_dir_kept_without_file() {
        assert_eq!(
            steps_of(
                "#[path = \"real.rs\"]\npub mod m;\nmod x;\n",
                "src/a/b.rs",
                false,
                &["src/a/b/x.rs", "src/a/b/m.rs"],
            ),
            [
                ("m".to_string(), PathBuf::from("src/a/real.rs")),
                ("x".to_string(), PathBuf::from("src/a/b/x.rs")),
            ]
        );
    }

    #[test]
    fn test_mod_steps_same_line_or_attributed_mods_take_no_step_empty_text_none() {
        let files = ["src/v.rs", "src/w.rs", "src/z.rs"];
        assert_eq!(
            steps_of("pub mod v; pub mod w;\n", "src/lib.rs", true, &files),
            [("v".to_string(), PathBuf::from("src/v.rs"))]
        );
        assert!(steps_of(
            "#[path = \"real.rs\"] pub mod z;\n",
            "src/lib.rs",
            true,
            &files
        )
        .is_empty());
        assert!(steps_of("// mod v;\n", "src/lib.rs", true, &files).is_empty());
        assert!(steps_of("", "src/lib.rs", true, &files).is_empty());
    }

    #[test]
    fn test_published_name_row_beats_glob_glob_answers_else_own() {
        let rows = pub_use_entries(
            &strip_comments("pub use a::f;\npub use b::*;\n"),
            &["m".to_string()],
        );
        let lookup = |name: &str| rows.get(name);
        let m = ["m".to_string()];
        assert_eq!(
            published(lookup, "f"),
            Published::ReExport(ReExport {
                prefix: &["a".to_string()],
                original: "f",
                containing: &m,
            })
        );
        assert_eq!(
            published(lookup, "g"),
            Published::ReExport(ReExport {
                prefix: &["b".to_string()],
                original: "*",
                containing: &m,
            })
        );
        let empty = PubUseEntries::default();
        assert_eq!(published(|name| empty.get(name), "f"), Published::Own);
    }
}
