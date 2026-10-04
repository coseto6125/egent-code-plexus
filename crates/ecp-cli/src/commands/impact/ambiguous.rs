//! `--ambiguous-callers`: the call sites of a name whose bare calls the
//! resolver suppressed at index time (`DecisionTier::AmbiguousGlobal`), found
//! by text match at query time. Persisting every suppressed site in graph.bin
//! was rejected (format bump; the noise volume is measured in
//! FU-2026-10-03-cab07766ef07), so the graph is
//! consulted here only to drop the sites it already explains.

use crate::git::safe_exec;
use ecp_core::file_category::Language;
use ecp_core::graph::RelType;
use ecp_core::session::MergedGraph;
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

pub(super) const MAX_SITES: usize = 50;

const NOTE: &str = "candidate call sites by text match (git grep), not resolved callers; \
                    not counted in the blast radius. Ruby calls without parentheses are not matched.";

struct Hit {
    path: String,
    line: u32,
    form: &'static str,
    /// Call occurrences of the name on this line.
    count: u32,
}

struct Scope<'a> {
    idx: u32,
    start: u32,
    end: u32,
    name: &'a str,
}

/// The `ambiguous_callers` payload object for `name` (the bare name, never
/// the `Owner.Method` form). `repo` is any directory inside the work tree.
pub(super) fn ambiguous_callers(merged: MergedGraph<'_>, name: &str, repo: &Path) -> Value {
    if name.trim().is_empty() {
        return failure("empty symbol name");
    }
    let defs = same_name_defs(merged, name);
    let pathspecs = pathspecs(merged, &defs, repo);
    if pathspecs.is_empty() {
        return sites_json(Vec::new());
    }
    let hits = match git_grep(repo, name, &pathspecs) {
        Ok(stdout) => parse_hits(&stdout, name.as_bytes()),
        Err(reason) => return failure(&reason),
    };
    if hits.is_empty() {
        return sites_json(Vec::new());
    }
    sites_json(unattributed(merged, &defs, hits))
}

fn failure(reason: &str) -> Value {
    json!({ "total": 0, "shown": 0, "note": NOTE, "error": reason, "sites": [] })
}

fn sites_json(mut hits: Vec<(Hit, Option<&str>)>) -> Value {
    hits.sort_by(|(a, _), (b, _)| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    let total: u32 = hits.iter().map(|(h, _)| h.count).sum();
    hits.truncate(MAX_SITES);
    let sites: Vec<Value> = hits
        .iter()
        .map(|(h, enclosing)| {
            let mut site =
                json!({ "file": h.path, "line": h.line, "enclosing": enclosing, "form": h.form });
            if h.count > 1 {
                site["count"] = h.count.into();
            }
            site
        })
        .collect();
    json!({ "total": total, "shown": sites.len(), "note": NOTE, "sites": sites })
}

/// Every on-disk definition named `name`, in merged space: base nodes the
/// overlay did not delete (redirected to their dirty twin) plus brand-new
/// working-tree nodes. Same population `resolve_candidates` counts.
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
                .filter(|(_, vn)| vn.replaced_base.is_none() && vn.name == name)
                .map(|(i, _)| base_len + i as u32),
        );
    }
    defs
}

fn extension_of(path: &str) -> Option<&str> {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => Some(ext),
        _ => None,
    }
}

/// Extensions of the tracked files that share a language with one of
/// `languages`, taken from the same extension -> language table the indexer
/// uses, so `.tsx` is searched when the definitions are in `.ts`.
fn family_extensions(repo: &Path, languages: &[Language]) -> BTreeSet<String> {
    let Ok(out) = safe_exec::git()
        .arg("-C")
        .arg(repo)
        .args(["ls-files", "-z", "--full-name", "--", ":(top)*"])
        .output()
    else {
        return BTreeSet::new();
    };
    out.stdout
        .split(|&b| b == 0)
        .filter_map(|p| extension_of(std::str::from_utf8(p).ok()?))
        .filter(|ext| languages.contains(&Language::from_path(&format!("x.{ext}"))))
        .map(str::to_string)
        .collect()
}

/// One pathspec per distinct extension of the definitions' files and of their
/// language family, rooted at the top of the work tree so a subdirectory
/// `--repo` still searches the whole repository the graph covers.
fn pathspecs(merged: MergedGraph<'_>, defs: &[u32], repo: &Path) -> Vec<String> {
    let paths: Vec<&str> = defs
        .iter()
        .filter_map(|&d| merged.node(d)?.file_path(&merged))
        .collect();
    let mut languages: Vec<Language> = Vec::new();
    for lang in paths.iter().map(|p| Language::from_path(p)) {
        if lang != Language::Unknown && !languages.contains(&lang) {
            languages.push(lang);
        }
    }
    let mut extensions: BTreeSet<String> = if languages.is_empty() {
        BTreeSet::new()
    } else {
        family_extensions(repo, &languages)
    };
    let mut specs: BTreeSet<String> = BTreeSet::new();
    for path in paths {
        match extension_of(path) {
            Some(ext) => {
                extensions.insert(ext.to_string());
            }
            None => {
                let base = path.rsplit('/').next().unwrap_or(path);
                specs.insert(format!(":(top,glob)**/{base}"));
            }
        }
    }
    specs.extend(extensions.into_iter().map(|ext| format!(":(top)*.{ext}")));
    specs.into_iter().collect()
}

fn ere_escape(name: &str) -> String {
    let mut out = String::with_capacity(name.len() * 2);
    for c in name.chars() {
        if "\\.^$*+?()[]{}|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn git_grep(repo: &Path, name: &str, pathspecs: &[String]) -> Result<Vec<u8>, String> {
    let pattern = format!(r"(^|[^A-Za-z0-9_$]){}[[:space:]]*\(", ere_escape(name));
    let out = safe_exec::git()
        .arg("-C")
        .arg(repo)
        .args([
            "grep",
            "--no-color",
            "--no-textconv",
            "--full-name",
            "-n",
            "-z",
            "-I",
            "-E",
            "-e",
        ])
        .arg(&pattern)
        .arg("--")
        .args(pathspecs)
        .output()
        .map_err(|e| format!("git did not run: {e}"))?;
    // git grep exits 1 for "no match", which is an answer, not a failure.
    match out.status.code() {
        Some(0) => Ok(out.stdout),
        Some(1) if out.stderr.is_empty() => Ok(Vec::new()),
        _ => Err(format!(
            "git grep failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Parse `git grep -n -z` records (`path\0line\0content\n`).
fn parse_hits(stdout: &[u8], name: &[u8]) -> Vec<Hit> {
    stdout
        .split(|&b| b == b'\n')
        .filter_map(|record| {
            let mut fields = record.splitn(3, |&b| b == 0);
            let path = std::str::from_utf8(fields.next()?).ok()?;
            let line = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
            let (form, count) = call_form(fields.next()?, name)?;
            Some(Hit {
                path: path.to_string(),
                line,
                form,
                count,
            })
        })
        .collect()
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

/// `("member", n)` for `x.name(` / `p->name(` / `T::name(`, `("bare", n)` for
/// `name(`, `None` when no whole-identifier call of `name` is on the line.
/// The form is the first call's; `n` counts every call on the line. A call is
/// whole-identifier when no Unicode letter, digit, `_` or `$` precedes it.
/// Mirrors the git pattern (which is looser on non-ASCII) so both agree on
/// what a match is.
fn call_form(content: &[u8], name: &[u8]) -> Option<(&'static str, u32)> {
    if name.is_empty() {
        return None;
    }
    let mut first = None;
    let mut count = 0;
    let mut from = 0;
    while let Some(off) = content[from..].windows(name.len()).position(|w| w == name) {
        let at = from + off;
        let end = at + name.len();
        let boundary = last_char(&content[..at]).is_none_or(|c| !is_ident_char(c));
        let paren = content[end..].iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'(');
        if boundary && paren {
            let prefix = &content[..at];
            let member =
                prefix.ends_with(b".") || prefix.ends_with(b"->") || prefix.ends_with(b"::");
            first.get_or_insert(if member { "member" } else { "bare" });
            count += 1;
        }
        from = at + 1;
    }
    first.map(|form| (form, count))
}

/// Drop the hits the graph already explains: a definition's own line, and a
/// call inside a function that already holds a `Calls` edge to `name`.
fn unattributed<'a>(
    merged: MergedGraph<'a>,
    defs: &[u32],
    hits: Vec<Hit>,
) -> Vec<(Hit, Option<&'a str>)> {
    let hit_paths: FxHashSet<&str> = hits.iter().map(|h| h.path.as_str()).collect();
    let scopes = scopes_in(merged, &hit_paths);

    let mut callers: FxHashSet<u32> = FxHashSet::default();
    // A construction lands on the type's constructor, which impact already
    // counts as the type's callers, so those sources are explained too.
    for &d in defs {
        for target in std::iter::once(d).chain(merged.constructors_of(d)) {
            callers.extend(
                merged
                    .in_edges(target)
                    .filter(|e| e.rel_type() == RelType::Calls)
                    .map(|e| e.source),
            );
        }
    }
    // A caller that is not a function (a File / Module node) stands for the
    // file's top level, so top-level hits in its file are already shown.
    let file_level_callers: FxHashSet<&str> = callers
        .iter()
        .filter_map(|&c| merged.node(c))
        .filter(|n| !n.kind().is_callable())
        .filter_map(|n| n.file_path(&merged))
        .collect();

    let mut def_lines: FxHashSet<(&str, u32)> = FxHashSet::default();
    let mut callable_defs: Vec<(&str, u32, u32)> = Vec::new();
    for &d in defs {
        let Some(node) = merged.node(d) else { continue };
        let Some(path) = node.file_path(&merged) else {
            continue;
        };
        def_lines.insert((path, node.start_line()));
        if node.kind().is_callable() {
            callable_defs.push((path, node.start_line(), node.end_line()));
        }
    }
    // A span can start on a decorator / annotation line, so a callable
    // definition's signature is the first hit inside its span. The cost is a
    // self-recursive call on a signature that does not match (`fn name<T>(`).
    for &(path, start, end) in &callable_defs {
        if let Some(first) = hits
            .iter()
            .filter(|h| h.path == path && (start..=end).contains(&h.line))
            .map(|h| h.line)
            .min()
        {
            def_lines.insert((path, first));
        }
    }

    hits.into_iter()
        .filter(|h| !def_lines.contains(&(h.path.as_str(), h.line)))
        .filter_map(|h| {
            let mut containing: Vec<&Scope<'a>> = scopes
                .get(h.path.as_str())
                .map(|v| {
                    v.iter()
                        .filter(|s| (s.start..=s.end).contains(&h.line))
                        .collect()
                })
                .unwrap_or_default();
            containing.sort_by_key(|s| s.end.saturating_sub(s.start));
            if containing.is_empty() {
                return (!file_level_callers.contains(h.path.as_str())).then_some((h, None));
            }
            // Innermost outward, up to the first named callable: a lambda's
            // call is attributed to the named function around it, but a
            // named inner function is its own caller, so a known call in the
            // outer function does not explain a hit inside it.
            let named = containing
                .iter()
                .position(|s| !s.name.starts_with("<anonymous"));
            let chain = &containing[..named.map_or(containing.len(), |i| i + 1)];
            if chain.iter().any(|s| callers.contains(&s.idx)) {
                return None;
            }
            let enclosing = named.map(|i| containing[i].name);
            Some((h, enclosing))
        })
        .collect()
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
    fn test_call_form_bare_and_member_syntaxes_classified() {
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
            ("get = 1", None),
            ("forget() + get()", Some("bare")),
        ];
        for (line, want) in cases {
            assert_eq!(
                call_form(line.as_bytes(), b"get").map(|(form, _)| form),
                *want,
                "line: {line}"
            );
        }
        assert_eq!(
            call_form(b"pkg.Get ()", b"Get").map(|(form, _)| form),
            Some("member")
        );
    }

    #[test]
    fn test_call_form_several_calls_on_one_line_counted() {
        assert_eq!(call_form(b"get(); get();", b"get"), Some(("bare", 2)));
        assert_eq!(call_form(b"a.get(); get();", b"get"), Some(("member", 2)));
        assert_eq!(
            call_form(b"get(); getter(); x = get", b"get"),
            Some(("bare", 1))
        );
    }

    #[test]
    fn test_call_form_non_ascii_letter_before_name_is_not_a_boundary() {
        assert_eq!(call_form("éget()".as_bytes(), b"get"), None);
        assert_eq!(call_form("日get()".as_bytes(), b"get"), None);
        assert_eq!(call_form("é get()".as_bytes(), b"get"), Some(("bare", 1)));
        assert_eq!(call_form(b"\xffget()", b"get"), Some(("bare", 1)));
    }

    #[test]
    fn test_call_form_empty_name_matches_nothing() {
        assert_eq!(call_form(b"x()", b""), None);
    }

    #[test]
    fn test_ere_escape_metacharacters_escaped() {
        assert_eq!(ere_escape("a.b$c(d)"), r"a\.b\$c\(d\)");
        assert_eq!(ere_escape("plain_name"), "plain_name");
    }

    #[test]
    fn test_parse_hits_nul_records_parsed_and_non_calls_dropped() {
        let out =
            b"src/a.py\x002\x00    get()\nsrc/b.py\x007\x00getter()\nsrc/c:d.py\x001\x00o.get(1)\n";
        let hits = parse_hits(out, b"get");
        let got: Vec<(&str, u32, &str)> = hits
            .iter()
            .map(|h| (h.path.as_str(), h.line, h.form))
            .collect();
        assert_eq!(
            got,
            vec![("src/a.py", 2, "bare"), ("src/c:d.py", 1, "member")]
        );
    }
}
