//! `unexplained_refs`: the whole-word occurrences of a target's bare name,
//! found by one scan of the tracked files at query time, that no graph edge into a
//! same-named definition explains. A short caller list is where an agent
//! stops looking, and the references the graph misses there (a function
//! passed as a value, a template, a file of another language, a bare call
//! the resolver suppressed as ambiguous) are text the graph never linked.
//! Persisting every such site in graph.bin was rejected (format bump; the
//! noise volume is measured in FU-2026-10-03-cab07766ef07), so the graph is
//! consulted here only to drop the occurrences it already explains.

use crate::git::safe_exec;
use ecp_core::file_category::determine_category;
use ecp_core::graph::{FileCategory, NodeKind, RelType};
use ecp_core::session::MergedGraph;
use memchr::memmem::Finder;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The non-test direct caller count at or below which the scan runs by
/// default. The impact answers judged wrong or incomplete had 0 to 2
/// callers; a higher value runs one more scan per query of a
/// better-called symbol.
pub(super) const UNEXPLAINED_SCAN_MAX_CALLERS: usize = 2;

/// Sites listed in the payload. `total` and `files` count every site.
const MAX_SITES: usize = 20;

const NOTE: &str = "whole-word text matches in tracked files that the graph does not explain; \
                    read them before you treat the caller list as complete. They are not \
                    counted in the blast radius.";

/// Directories of third-party code, matched at any depth. An ambiguous
/// target drops its candidates in them too.
pub(super) const VENDORED_DIRS: [&str; 4] = ["node_modules", "vendor", ".venv", "site-packages"];

/// Build output directories, matched at any depth because a monorepo nests
/// one per package. `build` also covers Laravel's `public/build`.
const BUILD_DIRS: [&str; 2] = ["dist", "build"];

/// Generated files, copies and documents, by file-name suffix and by file
/// name: a minified bundle, a patch or a test snapshot repeats source names, a
/// document names a symbol in prose, and a lockfile names packages.
const SKIPPED_SUFFIXES: [&str; 8] = [
    ".min.js", ".patch", ".diff", ".snap", ".md", ".rst", ".txt", ".lock",
];
const SKIPPED_NAMES: [&str; 2] = ["package-lock.json", "pnpm-lock.yaml"];

/// The bytes `git grep -I` reads for a NUL to call a file binary.
const BINARY_PROBE_BYTES: usize = 8000;

/// Whether a directory of `path` (repo-relative, `/`-separated) is one of
/// [`VENDORED_DIRS`].
pub(super) fn is_vendored(path: &str) -> bool {
    path.split('/')
        .rev()
        .skip(1)
        .any(|dir| VENDORED_DIRS.contains(&dir))
}

struct Hit {
    path: String,
    line: u32,
    form: &'static str,
    /// Whole-word occurrences of the name on this line.
    count: u32,
}

struct Site<'g> {
    hit: Hit,
    /// The innermost named function around the site.
    enclosing: Option<&'g str>,
}

/// The answer of one scan: every unexplained site, source files first and
/// then tests, examples and the rest, each by path and line; or the reason
/// `git` gave no file list.
pub(super) struct UnexplainedRefs<'g> {
    sites: Vec<Site<'g>>,
    error: Option<String>,
}

impl<'g> UnexplainedRefs<'g> {
    fn failed(reason: String) -> Self {
        Self {
            sites: Vec::new(),
            error: Some(reason),
        }
    }

    /// Every unexplained occurrence, the ones past the site cap included.
    pub(super) fn total(&self) -> u32 {
        self.sites.iter().map(|s| s.hit.count).sum()
    }

    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The files that hold a site, the ones past the site cap included.
    pub(super) fn files(&self) -> BTreeSet<&str> {
        self.sites.iter().map(|s| s.hit.path.as_str()).collect()
    }

    /// The `unexplained_refs` payload object. A site whose line holds
    /// several occurrences carries `count`; it is omitted when 1.
    pub(super) fn to_json(&self) -> Value {
        let sites: Vec<Value> = self
            .sites
            .iter()
            .take(MAX_SITES)
            .map(|s| {
                let mut site = json!({
                    "path": s.hit.path,
                    "line": s.hit.line,
                    "form": s.hit.form,
                    "enclosing": s.enclosing,
                });
                if s.hit.count > 1 {
                    site["count"] = s.hit.count.into();
                }
                site
            })
            .collect();
        let mut out = json!({
            "total": self.total(),
            "files": self.files().len(),
            "shown": sites.len(),
            "sites": sites,
            "note": NOTE,
        });
        if let Some(reason) = &self.error {
            out["error"] = json!(reason);
        }
        out
    }
}

/// Scan the work tree that holds `repo` for `name`: the bare name, never the
/// `Owner.Method` form. A `git` that cannot run or fails gives an answer
/// with `error` set, never an `Err`: the impact answer stands without it.
pub(super) fn scan<'g>(merged: MergedGraph<'g>, name: &str, repo: &Path) -> UnexplainedRefs<'g> {
    if name.trim().is_empty() {
        return UnexplainedRefs::failed("empty symbol name".to_string());
    }
    let hits = match tracked_files(repo) {
        Ok((top, files)) => search(&top, &files, name.as_bytes()),
        Err(reason) => return UnexplainedRefs::failed(reason),
    };
    let mut sites = unexplained(merged, &same_name_defs(merged, name), hits);
    // The capped list shows production code before test fixtures that
    // repeat the name.
    sites.sort_by_cached_key(|s| {
        (
            determine_category(&s.hit.path) != FileCategory::Source,
            s.hit.path.clone(),
            s.hit.line,
        )
    });
    UnexplainedRefs { sites, error: None }
}

/// Every on-disk definition named `name`, of every kind, in merged space:
/// base nodes the overlay did not delete (redirected to their dirty twin)
/// plus brand-new working-tree nodes. The on-disk half of what
/// [`crate::commands::symbol_id::same_name_def_count`] counts, plus what it
/// skips: same-named definitions of a non-competing kind, later overloads and
/// lexically scoped defs. Their declarations match the scan too.
fn same_name_defs(merged: MergedGraph<'_>, name: &str) -> Vec<u32> {
    let view = merged.view();
    let mut defs: Vec<u32> = merged
        .name_candidates(name)
        .filter(|&i| {
            let node = &merged.nodes[i as usize];
            node.has_owning_file() && node.name.resolve(&merged.string_pool) == name
        })
        .filter_map(|i| view.map_or(Some(i), |v| v.redirect(i)))
        .collect();
    if let Some(v) = view {
        let base_len = merged.base_len();
        defs.extend(
            v.virtual_nodes()
                .iter()
                .enumerate()
                .filter(|(_, vn)| {
                    vn.replaced_base.is_none() && vn.has_owning_file() && vn.name == name
                })
                .map(|(i, _)| base_len + i as u32),
        );
    }
    defs
}

/// Whether the scan skips `path` (top-relative, `/`-separated): vendored
/// code and build output at any depth, generated files and documents.
fn is_skipped(path: &str) -> bool {
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or(path);
    SKIPPED_SUFFIXES.iter().any(|suffix| file.ends_with(suffix))
        || SKIPPED_NAMES.contains(&file)
        || parts.any(|dir| VENDORED_DIRS.contains(&dir) || BUILD_DIRS.contains(&dir))
}

/// The top of the work tree that holds `repo`, and every tracked file from
/// that top, so a subdirectory `--repo` still searches every file the graph
/// covers. No language filter: a template, a view or a frontend file of
/// another language is the gap.
fn tracked_files(repo: &Path) -> Result<(PathBuf, Vec<String>), String> {
    let top = git(repo, &["rev-parse", "--show-toplevel"])?;
    let top = PathBuf::from(String::from_utf8_lossy(&top).trim_end_matches(['\n', '\r']));
    let listed = git(&top, &["ls-files", "-z"])?;
    let files = listed
        .split(|&b| b == 0)
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter(|path| !path.is_empty() && !is_skipped(path))
        .map(str::to_string)
        .collect();
    Ok((top, files))
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = safe_exec::git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git did not run: {e}"))?;
    match out.status.success() {
        true => Ok(out.stdout),
        false => Err(format!(
            "git {} failed: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Every line of `files` under `top` with a whole-word occurrence of `name`.
/// In-process rather than `git grep`: on a 15k-file tree `git grep` took
/// 300-460 ms per query against 32-34 ms for one parallel pass (vscode).
fn search(top: &Path, files: &[String], name: &[u8]) -> Vec<Hit> {
    let finder = Finder::new(name);
    files
        .par_iter()
        .flat_map_iter(|rel| search_file(&top.join(rel), rel, &finder, name))
        .collect()
}

/// The hits of one file. A path that is gone, no regular file (a submodule,
/// a symlink) or binary by `git grep -I`'s test gives none.
fn search_file(path: &Path, rel: &str, finder: &Finder, name: &[u8]) -> Vec<Hit> {
    let regular = fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file());
    let Some(text) = regular.then(|| fs::read(path).ok()).flatten() else {
        return Vec::new();
    };
    if memchr::memchr(0, &text[..text.len().min(BINARY_PROBE_BYTES)]).is_some() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let (mut line, mut counted, mut line_end) = (1u32, 0usize, 0usize);
    for at in finder.find_iter(&text) {
        if at < line_end {
            continue;
        }
        line += memchr::memchr_iter(b'\n', &text[counted..at]).count() as u32;
        counted = at;
        let start = memchr::memrchr(b'\n', &text[..at]).map_or(0, |i| i + 1);
        line_end = memchr::memchr(b'\n', &text[at..]).map_or(text.len(), |i| at + i);
        if let Some((form, count)) = occurrences(&text[start..line_end], name) {
            hits.push(Hit {
                path: rel.to_string(),
                line,
                form,
                count,
            });
        }
    }
    hits
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The character that ends `bytes`, or `None` when `bytes` is empty or ends
/// in invalid UTF-8.
fn last_char(bytes: &[u8]) -> Option<char> {
    (1..=bytes.len().min(4)).find_map(|k| {
        std::str::from_utf8(&bytes[bytes.len() - k..])
            .ok()?
            .chars()
            .next()
    })
}

/// The character that starts `bytes`, or `None` when `bytes` is empty or
/// starts in invalid UTF-8.
fn first_char(bytes: &[u8]) -> Option<char> {
    (1..=bytes.len().min(4)).find_map(|k| std::str::from_utf8(&bytes[..k]).ok()?.chars().next())
}

/// The form of the first whole-word occurrence of `name` on the line, and
/// the number of whole-word occurrences; `None` when there is none. An
/// occurrence is whole-word when no Unicode letter, digit, `_` or `$` touches
/// it on either side.
/// The form is `member` for `x.name(` / `p->name(` / `T::name(`, `bare` for
/// `name(`, and `reference` for every other use: a value, a type, a
/// template, a string, a Ruby call without parentheses.
fn occurrences(content: &[u8], name: &[u8]) -> Option<(&'static str, u32)> {
    if name.is_empty() {
        return None;
    }
    let mut first = None;
    let mut count = 0;
    let mut from = 0;
    while let Some(off) = content[from..].windows(name.len()).position(|w| w == name) {
        let at = from + off;
        let end = at + name.len();
        let whole = last_char(&content[..at]).is_none_or(|c| !is_ident_char(c))
            && first_char(&content[end..]).is_none_or(|c| !is_ident_char(c));
        if !whole {
            from = at + 1;
            continue;
        }
        let prefix = &content[..at];
        let call = content[end..].iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'(');
        let member = prefix.ends_with(b".") || prefix.ends_with(b"->") || prefix.ends_with(b"::");
        first.get_or_insert(match (call, member) {
            (false, _) => "reference",
            (true, true) => "member",
            (true, false) => "bare",
        });
        count += 1;
        from = end;
    }
    first.map(|form| (form, count))
}

/// What the graph says about the occurrences: the sources of the edges into
/// a definition or into a type's constructor (impact counts a construction
/// as a caller of the type), and the files that hold an import of one.
struct Explainers<'g> {
    sources: FxHashSet<u32>,
    importing_files: FxHashSet<&'g str>,
}

fn explainers<'g>(merged: MergedGraph<'g>, defs: &[u32]) -> Explainers<'g> {
    let mut out = Explainers {
        sources: FxHashSet::default(),
        importing_files: FxHashSet::default(),
    };
    for &d in defs {
        for target in std::iter::once(d).chain(merged.constructors_of(d)) {
            for edge in merged.in_edges(target) {
                match edge.rel_type() {
                    RelType::Imports => out
                        .importing_files
                        .extend(merged.node(edge.source).and_then(|n| n.file_path(&merged))),
                    rel if rel.is_scope_containment() || rel == RelType::PartOf => {}
                    _ => {
                        out.sources.insert(edge.source);
                    }
                }
            }
        }
    }
    out
}

/// Drop the occurrences the graph already explains:
/// - a definition's declaration;
/// - an occurrence inside a callable that holds an edge to a definition
///   (the innermost named callable, with the lambdas inside it);
/// - an occurrence inside the span of a value declaration or a type that
///   holds such an edge, outside every named callable declared within it;
/// - every occurrence in the file of a File / Module node that holds one;
/// - the import statement of a file whose import the graph holds.
fn unexplained<'g>(merged: MergedGraph<'g>, defs: &[u32], hits: Vec<Hit>) -> Vec<Site<'g>> {
    if hits.is_empty() {
        return Vec::new();
    }
    let hit_paths: FxHashSet<&str> = hits.iter().map(|h| h.path.as_str()).collect();
    let scopes = scopes_in(merged, &hit_paths);
    let explainers = explainers(merged, defs);

    // Schema 56 makes value declarations and types the caller of the calls
    // outside functions, so only a File / Module stands for a whole file:
    // a wider rule would hide the occurrences in the file's other functions.
    let mut whole_files: FxHashSet<&str> = FxHashSet::default();
    let mut own_spans: FxHashMap<&str, Vec<(u32, u32)>> = FxHashMap::default();
    for node in explainers.sources.iter().filter_map(|&s| merged.node(s)) {
        let kind = node.kind();
        let Some(path) = node.file_path(&merged).filter(|_| !kind.is_callable()) else {
            continue;
        };
        if matches!(kind, NodeKind::File | NodeKind::Module) {
            whole_files.insert(path);
        } else {
            own_spans
                .entry(path)
                .or_default()
                .push((node.start_line(), node.end_line()));
        }
    }

    // A definition's first occurrence inside its span is its declaration: a
    // span can start on a decorator or annotation line.
    let first_in = |path: &str, keep: &dyn Fn(&Hit) -> bool| {
        hits.iter()
            .filter(|&h| h.path == path && keep(h))
            .map(|h| h.line)
            .min()
    };
    let mut explained: FxHashSet<(&str, u32)> = FxHashSet::default();
    for node in defs.iter().filter_map(|&d| merged.node(d)) {
        let Some(path) = node.file_path(&merged) else {
            continue;
        };
        let span = node.start_line()..=node.end_line();
        explained.insert((path, node.start_line()));
        explained
            .extend(first_in(path, &|h: &Hit| span.contains(&h.line)).map(|line| (path, line)));
    }
    // The graph keeps an import edge, not its line: the first occurrence
    // outside every callable of an importing file is the import statement.
    for &path in &explainers.importing_files {
        let top_level = |h: &Hit| containing(&scopes, path, h.line).is_empty();
        explained.extend(first_in(path, &top_level).map(|line| (path, line)));
    }

    hits.into_iter()
        .filter_map(|hit| {
            let path = hit.path.as_str();
            if whole_files.contains(path) || explained.contains(&(path, hit.line)) {
                return None;
            }
            let around = containing(&scopes, path, hit.line);
            // Innermost outward, up to the first named callable: a lambda's
            // occurrence is attributed to the named function around it, but
            // a named inner function is its own caller, so an edge from the
            // outer function does not explain an occurrence inside it.
            let named = around
                .iter()
                .position(|s| !s.name.starts_with("<anonymous"));
            let chain = &around[..named.map_or(around.len(), |i| i + 1)];
            if chain.iter().any(|s| explainers.sources.contains(&s.idx)) {
                return None;
            }
            let named = named.map(|i| around[i]);
            let in_own_span = own_spans.get(path).is_some_and(|spans| {
                spans.iter().any(|&(start, end)| {
                    (start..=end).contains(&hit.line)
                        && named.is_none_or(|n| n.start < start || n.end > end)
                })
            });
            if in_own_span {
                return None;
            }
            let enclosing = named.map(|n| n.name);
            Some(Site { hit, enclosing })
        })
        .collect()
}

struct Scope<'a> {
    idx: u32,
    start: u32,
    end: u32,
    name: &'a str,
}

/// The callable scopes of `path` that contain `line`, innermost first.
fn containing<'s, 'g>(
    scopes: &'s FxHashMap<&'g str, Vec<Scope<'g>>>,
    path: &str,
    line: u32,
) -> Vec<&'s Scope<'g>> {
    let mut found: Vec<&Scope<'g>> = scopes
        .get(path)
        .map(|v| {
            v.iter()
                .filter(|s| (s.start..=s.end).contains(&line))
                .collect()
        })
        .unwrap_or_default();
    found.sort_by_key(|s| s.end.saturating_sub(s.start));
    found
}

/// Function / Method / Constructor spans in the hit files, merged-space.
fn scopes_in<'a>(
    merged: MergedGraph<'a>,
    hit_paths: &FxHashSet<&str>,
) -> FxHashMap<&'a str, Vec<Scope<'a>>> {
    let hit_file: Vec<bool> = merged
        .files
        .iter()
        .map(|f| hit_paths.contains(f.path.resolve(&merged.string_pool)))
        .collect();
    let view = merged.view();
    let mut scopes: FxHashMap<&'a str, Vec<Scope<'a>>> = FxHashMap::default();
    let push = |idx: u32, scopes: &mut FxHashMap<&'a str, Vec<Scope<'a>>>| {
        let Some(node) = merged.node(idx) else { return };
        if !node.kind().is_callable() {
            return;
        }
        let Some(path) = node.file_path(&merged) else {
            return;
        };
        scopes.entry(path).or_default().push(Scope {
            idx,
            start: node.start_line(),
            end: node.end_line(),
            name: node.name(&merged),
        });
    };
    for (i, node) in merged.nodes.iter().enumerate() {
        if !node.has_owning_file() || !hit_file[node.file_idx.to_native() as usize] {
            continue;
        }
        if let Some(idx) = view.map_or(Some(i as u32), |v| v.redirect(i as u32)) {
            push(idx, &mut scopes);
        }
    }
    if let Some(v) = view {
        let base_len = merged.base_len();
        for (i, vn) in v.virtual_nodes().iter().enumerate() {
            if vn.replaced_base.is_none() && hit_paths.contains(&*vn.rel_path) {
                push(base_len + i as u32, &mut scopes);
            }
        }
    }
    scopes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_occurrences_call_member_and_reference_forms_classified() {
        let cases: &[(&str, Option<&str>)] = &[
            ("    get()", Some("bare")),
            ("x = obj.get(1)", Some("member")),
            ("pkg.Get ()", None),
            ("$x->get($y)", Some("member")),
            ("Type::get(a)", Some("member")),
            ("p->get(a)", Some("member")),
            ("getter()", None),
            ("_get()", None),
            ("$get()", None),
            ("get_all = 1", None),
            ("handlers = [get]", Some("reference")),
            ("{{ get }}", Some("reference")),
            ("obj.get a", Some("reference")),
            ("forget() + get()", Some("bare")),
        ];
        for (line, want) in cases {
            assert_eq!(
                occurrences(line.as_bytes(), b"get").map(|(form, _)| form),
                *want,
                "line: {line}"
            );
        }
        assert_eq!(
            occurrences(b"pkg.Get ()", b"Get").map(|(form, _)| form),
            Some("member")
        );
    }

    #[test]
    fn test_occurrences_several_on_one_line_counted() {
        assert_eq!(occurrences(b"get(); get();", b"get"), Some(("bare", 2)));
        assert_eq!(occurrences(b"a.get(); get;", b"get"), Some(("member", 2)));
        assert_eq!(
            occurrences(b"x = get; getter(); get()", b"get"),
            Some(("reference", 2))
        );
    }

    #[test]
    fn test_occurrences_non_ascii_letter_touching_name_is_not_a_boundary() {
        assert_eq!(occurrences("éget()".as_bytes(), b"get"), None);
        assert_eq!(occurrences("日get()".as_bytes(), b"get"), None);
        assert_eq!(occurrences("geté".as_bytes(), b"get"), None);
        assert_eq!(occurrences("é get()".as_bytes(), b"get"), Some(("bare", 1)));
        assert_eq!(occurrences(b"\xffget()", b"get"), Some(("bare", 1)));
        assert_eq!(occurrences(b"get\xff", b"get"), Some(("reference", 1)));
    }

    #[test]
    fn test_occurrences_empty_name_matches_nothing() {
        assert_eq!(occurrences(b"x()", b""), None);
    }

    #[test]
    fn test_occurrences_regex_metacharacters_in_name_matched_literally() {
        assert_eq!(
            occurrences(b"call(a.b$c)", b"a.b$c"),
            Some(("reference", 1))
        );
        assert_eq!(occurrences(b"axb$c", b"a.b$c"), None);
    }

    fn hits_of(text: &[u8], name: &[u8]) -> Vec<(u32, &'static str, u32)> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, text).unwrap();
        search_file(&path, "f", &Finder::new(name), name)
            .iter()
            .map(|h| (h.line, h.form, h.count))
            .collect()
    }

    #[test]
    fn test_search_file_lines_forms_and_partial_words() {
        let text = b"get()\ngetter()\n  x.get(1) + get(2)\r\n\nreturn get";
        assert_eq!(
            hits_of(text, b"get"),
            vec![(1, "bare", 1), (3, "member", 2), (5, "reference", 1)]
        );
    }

    #[test]
    fn test_search_file_empty_binary_and_unmatched_give_none() {
        assert_eq!(hits_of(b"", b"get"), vec![]);
        assert_eq!(hits_of(b"x\0get()\n", b"get"), vec![]);
        assert_eq!(hits_of(b"getter\nforget\n", b"get"), vec![]);
    }

    #[test]
    fn test_search_file_nul_past_probe_still_searched() {
        let mut text = vec![b'a'; BINARY_PROBE_BYTES];
        text.extend_from_slice(b"\n\0 get()\n");
        assert_eq!(hits_of(&text, b"get"), vec![(2, "bare", 1)]);
    }

    #[test]
    fn test_search_file_directory_and_missing_path_give_none() {
        let dir = tempfile::tempdir().unwrap();
        let finder = Finder::new(b"get");
        assert!(search_file(dir.path(), "d", &finder, b"get").is_empty());
        assert!(search_file(&dir.path().join("gone"), "gone", &finder, b"get").is_empty());
    }

    #[test]
    fn test_is_vendored_directory_segments_only() {
        assert!(is_vendored("node_modules/x/i.js"));
        assert!(is_vendored("app/vendor/lib.php"));
        assert!(is_vendored(".venv/lib/python3.12/site-packages/x.py"));
        assert!(!is_vendored("src/vendor.py"));
        assert!(!is_vendored("vendor"));
        assert!(!is_vendored("src/vendors/x.go"));
    }

    #[test]
    fn test_is_skipped_vendored_build_generated_and_documents() {
        for path in [
            "node_modules/x/i.js",
            "pkg/dist/a.js",
            "public/build/app.js",
            "a/b.min.js",
            "fixtures/x.patch",
            "a.diff",
            "__snapshots__/a.test.ts.snap",
            "README.md",
            "docs/x.rst",
            "notes.txt",
            "Cargo.lock",
            "web/package-lock.json",
            "pnpm-lock.yaml",
        ] {
            assert!(is_skipped(path), "{path}");
        }
        for path in [
            "src/build.rs",
            "src/dist.py",
            "src/lock.py",
            "src/app.js",
            "build",
        ] {
            assert!(!is_skipped(path), "{path}");
        }
    }
}
