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
use crate::session::sources::open_admitted;
use ecp_core::contracts::routing::SizeCap;
use ecp_core::file_category::determine_category;
use ecp_core::graph::{FileCategory, NodeKind, RelType};
use ecp_core::session::MergedGraph;
use memchr::memmem::Finder;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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

/// `git grep` had no size cap, so the scan reads a file of any length.
fn no_size_cap() -> SizeCap {
    SizeCap::resolve(None, Some(u64::MAX))
}

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
    /// Shared by every hit of one file.
    path: Arc<str>,
    /// Whether `path` is a source file, decided once per file.
    source: bool,
    line: u32,
    form: &'static str,
    /// The first occurrence binds the name: `name =` or `name:`, as a
    /// `const name = () =>`, a `name = lambda` or a `val name: () -> T` does.
    binds: bool,
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
    /// Tracked files the scan could not read: their text is not in `sites`.
    unread: usize,
}

impl<'g> UnexplainedRefs<'g> {
    pub(super) fn failed(reason: String) -> Self {
        Self {
            sites: Vec::new(),
            error: Some(reason),
            unread: 0,
        }
    }

    /// Why a zero `total` proves nothing: the scan failed, or it left
    /// tracked files unread and found nothing in the rest.
    pub(super) fn inconclusive(&self) -> Option<String> {
        match (&self.error, self.total(), self.unread) {
            (Some(reason), ..) => Some(reason.clone()),
            (None, 0, n @ 1..) => Some(format!("{n} tracked file(s) could not be read")),
            _ => None,
        }
    }

    /// Every unexplained occurrence, the ones past the site cap included.
    pub(super) fn total(&self) -> u32 {
        self.sites.iter().map(|s| s.hit.count).sum()
    }

    /// The files that hold a site, the ones past the site cap included.
    pub(super) fn files(&self) -> BTreeSet<&str> {
        self.sites.iter().map(|s| &*s.hit.path).collect()
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
                    "path": &*s.hit.path,
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
        if self.unread > 0 {
            out["unread"] = self.unread.into();
        }
        out
    }
}

/// Scan the work tree that holds `repo` for `name`: the bare name, never the
/// `Owner.Method` form. A `git` that cannot run or fails gives an answer
/// with `error` set, never an `Err`: the impact answer stands without it. A
/// tracked file the scan cannot read is counted in `unread`, not skipped
/// silently.
pub(super) fn scan<'g>(merged: MergedGraph<'g>, name: &str, repo: &Path) -> UnexplainedRefs<'g> {
    if name.trim().is_empty() {
        return UnexplainedRefs::failed("empty symbol name".to_string());
    }
    let (hits, unread) = match tracked_files(repo) {
        Ok(tracked) => {
            let (hits, unread) = search(&tracked.top, &tracked.files, name.as_bytes());
            (hits, unread + tracked.unlisted)
        }
        Err(reason) => return UnexplainedRefs::failed(reason),
    };
    let mut sites = unexplained(merged, &same_name_defs(merged, name), hits);
    // The capped list shows production code before test fixtures that
    // repeat the name.
    sites.sort_unstable_by(|a, b| {
        (!a.hit.source, &a.hit.path, a.hit.line).cmp(&(!b.hit.source, &b.hit.path, b.hit.line))
    });
    UnexplainedRefs {
        sites,
        error: None,
        unread,
    }
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

/// The tracked files the scan reads, and what it could not list.
struct Tracked {
    top: PathBuf,
    files: Vec<String>,
    /// Listed paths that are not valid UTF-8: they cannot be read as `files`.
    unlisted: usize,
}

/// The top of the work tree that holds `repo`, and every tracked file from
/// that top, so a subdirectory `--repo` still searches every file the graph
/// covers. No language filter: a template, a view or a frontend file of
/// another language is the gap.
fn tracked_files(repo: &Path) -> Result<Tracked, String> {
    // The cached layout avoids a `git rev-parse` spawn; the spawn stays for
    // a layout the cache does not model.
    let top = match crate::git_cache::git_layout(repo) {
        Some((top, ..)) => top,
        None => {
            let out = git(repo, &["rev-parse", "--show-toplevel"])?;
            let top = String::from_utf8(out)
                .map_err(|_| "work tree path is not valid UTF-8".to_string())?;
            PathBuf::from(top.trim_end_matches(['\n', '\r']))
        }
    };
    let listed = git(&top, &["ls-files", "-z"])?;
    let mut unlisted = 0;
    let mut files: Vec<String> = Vec::new();
    for path in listed.split(|&b| b == 0).filter(|p| !p.is_empty()) {
        match std::str::from_utf8(path) {
            Ok(path) if !is_skipped(path) => files.push(path.to_string()),
            Ok(_) => {}
            Err(_) if is_skipped(&String::from_utf8_lossy(path)) => {}
            Err(_) => unlisted += 1,
        }
    }
    // A conflicted path is listed once per index stage.
    files.dedup();
    Ok(Tracked {
        top,
        files,
        unlisted,
    })
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

/// Every line of `files` under `top` with a whole-word occurrence of `name`,
/// and the number of files that could not be read.
/// In-process rather than `git grep`: on a 15k-file tree `git grep` took
/// 300-460 ms per query against 32-34 ms for one parallel pass (vscode).
fn search(top: &Path, files: &[String], name: &[u8]) -> (Vec<Hit>, usize) {
    let finder = Finder::new(name);
    let unread = AtomicUsize::new(0);
    let hits = files
        .par_iter()
        .flat_map_iter(|rel| {
            search_file(&top.join(rel), rel, &finder, name).unwrap_or_else(|| {
                unread.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            })
        })
        .collect();
    (hits, unread.into_inner())
}

/// The hits of one file; `None` when the file exists as a regular file but
/// cannot be read. A path that is gone, no regular file (a submodule, a
/// symlink) or binary by `git grep -I`'s test gives no hits.
fn search_file(path: &Path, rel: &str, finder: &Finder, name: &[u8]) -> Option<Vec<Hit>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Some(Vec::new()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    }
    // Opened without blocking, with the type checked on the open handle: a
    // path swapped for a FIFO since the `lstat` is refused, not waited on.
    let (mut file, len) = open_admitted(path, no_size_cap)?;
    // The whole length is reserved only for a text file: a large binary
    // stops at the probe.
    let mut text = Vec::with_capacity(BINARY_PROBE_BYTES.min(len as usize));
    (&mut file)
        .take(BINARY_PROBE_BYTES as u64)
        .read_to_end(&mut text)
        .ok()?;
    if memchr::memchr(0, &text).is_some() {
        return Some(Vec::new());
    }
    text.reserve((len as usize).saturating_sub(text.len()));
    file.read_to_end(&mut text).ok()?;
    let mut hits = Vec::new();
    let mut shared: Option<(Arc<str>, bool)> = None;
    let (mut line, mut counted, mut line_end) = (1u32, 0usize, 0usize);
    for at in finder.find_iter(&text) {
        if at < line_end {
            continue;
        }
        line += memchr::memchr_iter(b'\n', &text[counted..at]).count() as u32;
        counted = at;
        let start = memchr::memrchr(b'\n', &text[..at]).map_or(0, |i| i + 1);
        line_end = memchr::memchr(b'\n', &text[at..]).map_or(text.len(), |i| at + i);
        if let Some((form, binds, count)) = occurrences(&text[start..line_end], name) {
            let (path, source) = shared.get_or_insert_with(|| {
                (
                    Arc::from(rel),
                    determine_category(rel) == FileCategory::Source,
                )
            });
            hits.push(Hit {
                path: Arc::clone(path),
                source: *source,
                line,
                form,
                binds,
                count,
            });
        }
    }
    Some(hits)
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
/// template, a string, a Ruby call without parentheses. `binds` is true
/// when the first occurrence is followed by `=` or `:` (not `==`, `=>`, `::`).
fn occurrences(content: &[u8], name: &[u8]) -> Option<(&'static str, bool, u32)> {
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
        let rest = &content[end..];
        let next = rest.iter().position(|b| !b.is_ascii_whitespace());
        let after = |k: usize| next.and_then(|i| rest.get(i + k)).copied();
        let call = after(0) == Some(b'(');
        let binds = match (after(0), after(1)) {
            (Some(b'='), Some(b'=' | b'>')) | (Some(b':'), Some(b':')) => false,
            (Some(b'=' | b':'), _) => true,
            _ => false,
        };
        let member = prefix.ends_with(b".") || prefix.ends_with(b"->") || prefix.ends_with(b"::");
        first.get_or_insert((
            match (call, member) {
                (false, _) => "reference",
                (true, true) => "member",
                (true, false) => "bare",
            },
            binds,
        ));
        count += 1;
        from = end;
    }
    first.map(|(form, binds)| (form, binds, count))
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
/// - a definition's declaration, one occurrence: a binding that is the first
///   match in a callable's span, else its first `bare` match with every match
///   before it (an annotation or decorator that holds the name), or the first
///   match of any other definition;
/// - an occurrence inside a callable that holds an edge to a definition
///   (the innermost named callable, with the lambdas inside it);
/// - an occurrence inside the span of a value declaration or a type that
///   holds such an edge, outside every named callable declared within it;
/// - every occurrence in the file of a File / Module node that holds one;
/// - the import statement of a file whose import the graph holds, one
///   occurrence.
///
/// A declaration and an import statement explain one occurrence each, so a
/// second occurrence on the same line stays; an edge explains a whole span.
fn unexplained<'g>(merged: MergedGraph<'g>, defs: &[u32], hits: Vec<Hit>) -> Vec<Site<'g>> {
    if hits.is_empty() {
        return Vec::new();
    }
    // Hits of a file are in line order.
    let mut by_path: FxHashMap<&str, Vec<&Hit>> = FxHashMap::default();
    for hit in &hits {
        by_path.entry(&hit.path).or_default().push(hit);
    }
    let hit_paths: FxHashSet<&str> = by_path.keys().copied().collect();
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

    let mut spent: FxHashMap<(&str, u32), u32> = FxHashMap::default();
    let mut spend = |path: &'g str, line: u32, n: u32| {
        let slot = spent.entry((path, line)).or_default();
        *slot = slot.saturating_add(n);
    };
    for node in defs.iter().filter_map(|&d| merged.node(d)) {
        let Some(path) = node.file_path(&merged) else {
            continue;
        };
        let file_hits = by_path.get(path).map_or(&[][..], Vec::as_slice);
        let from = file_hits.partition_point(|h| h.line < node.start_line());
        let within = || {
            file_hits[from..]
                .iter()
                .take_while(|h| h.line <= node.end_line())
        };
        // A binding declares only as the span's first hit: `total = x` in
        // the body of a Ruby `def total` is a local, not the declaration.
        let first = within().next();
        let declaration = match node.kind().is_callable() {
            true => first
                .filter(|h| h.binds)
                .or_else(|| within().find(|h| h.form == "bare"))
                .or(first),
            false => first,
        };
        if let Some(declaration) = declaration {
            for before in within().take_while(|h| h.line < declaration.line) {
                spend(path, before.line, u32::MAX);
            }
            spend(path, declaration.line, 1);
        }
    }
    // The graph keeps an import edge, not its line: the first occurrence
    // outside every callable of an importing file is the import statement.
    for &path in &explainers.importing_files {
        let top_level = by_path.get(path).and_then(|v| {
            v.iter()
                .find(|h| containing(&scopes, path, h.line).is_empty())
        });
        if let Some(hit) = top_level {
            spend(path, hit.line, 1);
        }
    }

    hits.into_iter()
        .filter_map(|mut hit| {
            let path = &*hit.path;
            let remaining = hit
                .count
                .saturating_sub(spent.get(&(path, hit.line)).copied().unwrap_or(0));
            if whole_files.contains(path) || remaining == 0 {
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
            hit.count = remaining;
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
                occurrences(line.as_bytes(), b"get").map(|(form, _, _)| form),
                *want,
                "line: {line}"
            );
        }
        assert_eq!(
            occurrences(b"pkg.Get ()", b"Get").map(|(form, _, _)| form),
            Some("member")
        );
    }

    #[test]
    fn test_occurrences_several_on_one_line_counted() {
        assert_eq!(
            occurrences(b"get(); get();", b"get"),
            Some(("bare", false, 2))
        );
        assert_eq!(
            occurrences(b"a.get(); get;", b"get"),
            Some(("member", false, 2))
        );
        assert_eq!(
            occurrences(b"x = get; getter(); get()", b"get"),
            Some(("reference", false, 2))
        );
    }

    #[test]
    fn test_occurrences_non_ascii_letter_touching_name_is_not_a_boundary() {
        assert_eq!(occurrences("éget()".as_bytes(), b"get"), None);
        assert_eq!(occurrences("日get()".as_bytes(), b"get"), None);
        assert_eq!(occurrences("geté".as_bytes(), b"get"), None);
        assert_eq!(
            occurrences("é get()".as_bytes(), b"get"),
            Some(("bare", false, 1))
        );
        assert_eq!(occurrences(b"\xffget()", b"get"), Some(("bare", false, 1)));
        assert_eq!(
            occurrences(b"get\xff", b"get"),
            Some(("reference", false, 1))
        );
    }

    #[test]
    fn test_occurrences_binding_first_occurrence_binds() {
        let binds = |line: &str| occurrences(line.as_bytes(), b"get").map(|(_, b, _)| b);
        for line in [
            "const get = (n) => n",
            "get = lambda n: n",
            "val get: () -> Int = { 1 }",
        ] {
            assert_eq!(binds(line), Some(true), "line: {line}");
        }
        for line in [
            "if get == 1",
            "get => 1",
            "Foo::get::<T>",
            "f = get",
            "get(1)",
        ] {
            assert_eq!(binds(line), Some(false), "line: {line}");
        }
    }

    #[test]
    fn test_occurrences_empty_name_matches_nothing() {
        assert_eq!(occurrences(b"x()", b""), None);
    }

    #[test]
    fn test_occurrences_regex_metacharacters_in_name_matched_literally() {
        assert_eq!(
            occurrences(b"call(a.b$c)", b"a.b$c"),
            Some(("reference", false, 1))
        );
        assert_eq!(occurrences(b"axb$c", b"a.b$c"), None);
    }

    fn hits_of(text: &[u8], name: &[u8]) -> Vec<(u32, &'static str, u32)> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, text).unwrap();
        search_file(&path, "f", &Finder::new(name), name)
            .unwrap()
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
        assert!(search_file(dir.path(), "d", &finder, b"get")
            .unwrap()
            .is_empty());
        assert!(
            search_file(&dir.path().join("gone"), "gone", &finder, b"get")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_search_file_fifo_path_gives_no_hits_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.is_ok_and(|s| s.success()) {
            return;
        }
        // A FIFO is no regular file: `symlink_metadata` skips it before any open.
        assert!(search_file(&fifo, "pipe", &Finder::new(b"get"), b"get")
            .unwrap()
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_search_file_unreadable_regular_file_is_none() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        fs::write(&path, b"get()\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&path).is_ok() {
            return; // root ignores the mode
        }
        assert!(search_file(&path, "secret", &Finder::new(b"get"), b"get").is_none());
    }

    #[test]
    fn test_tracked_files_conflicted_path_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
        };
        run(&["init", "-q", "-b", "main"]);
        fs::write(dir.path().join("a.js"), "get()\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "base"]);
        run(&["checkout", "-qb", "other"]);
        fs::write(dir.path().join("a.js"), "get(1)\n").unwrap();
        run(&["commit", "-qam", "other"]);
        run(&["checkout", "-q", "main"]);
        fs::write(dir.path().join("a.js"), "get(2)\n").unwrap();
        run(&["commit", "-qam", "main"]);
        assert!(!run(&["merge", "other"]).status.success());
        let tracked = tracked_files(dir.path()).unwrap();
        assert_eq!(tracked.files, vec!["a.js".to_string()]);
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
