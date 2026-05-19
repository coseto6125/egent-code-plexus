//! Extract static fetch URL literals from consumer source files.
//!
//! Companion to [`consumer_keys`](super::consumer_keys) — that module
//! pulls accessed keys; this one pulls the URLs whose responses are
//! being consumed. Together they let the builder emit
//! `RelType::Fetches` edges (consumer file → matching Route node).
//!
//! Patterns recognised:
//! - `fetch('/path')` / `fetch("/path")` (Fetch API)
//! - `axios.get|post|put|delete|patch('/path')` (axios)
//! - `$.get|post('/path')` / `$.ajax({ url: '/path' })` (jQuery — bare URL only)
//! - `requests.get|post|...('/path')` (Python `requests` consumed from JS bridge — rare but supported by upstream)
//!
//! URLs are returned verbatim — no normalisation. The caller decides
//! how strict (exact-match vs template-aware) the route-lookup is.
//! Quoted strings only; concatenations / template-literal expressions
//! that the upstream `normalizeFetchURL` would reject also produce
//! nothing useful here (we just don't emit them).

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// `fetch("…")` / `fetch('…')` / `fetch(\`…\`)` — first arg only.
/// Template literals without `${…}` interpolation are accepted; with
/// interpolation the URL is unstable and we drop it (caller never
/// finds a match in the route map anyway).
static FETCH_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\bfetch\s*\(\s*['"`]([^'"`$\s]+)['"`]"#).expect("FETCH_CALL compiles")
});

/// `axios.get('/x')` / `axios.post("/x")` — covers GET/POST/PUT/DELETE/PATCH.
static AXIOS_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\baxios\s*\.\s*(?:get|post|put|delete|patch)\s*\(\s*['"`]([^'"`$\s]+)['"`]"#)
        .expect("AXIOS_CALL compiles")
});

/// `$.get('/x')` / `$.post('/x')` — bare-URL jQuery shorthand.
static JQUERY_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\$\s*\.\s*(?:get|post)\s*\(\s*['"`]([^'"`$\s]+)['"`]"#)
        .expect("JQUERY_CALL compiles")
});

/// Extract every statically-resolvable fetch URL from `content`.
/// Output is deduped (set semantics) and sorted for stable downstream
/// behaviour. Empty when no patterns match.
pub fn extract(content: &str) -> Vec<String> {
    let mut urls: HashSet<String> = HashSet::new();

    for caps in FETCH_CALL.captures_iter(content) {
        urls.insert(caps[1].to_string());
    }
    for caps in AXIOS_CALL.captures_iter(content) {
        urls.insert(caps[1].to_string());
    }
    for caps in JQUERY_CALL.captures_iter(content) {
        urls.insert(caps[1].to_string());
    }

    let mut out: Vec<String> = urls.into_iter().collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty() {
        assert!(extract("").is_empty());
    }

    #[test]
    fn fetch_double_quoted() {
        assert_eq!(extract(r#"fetch("/api/users")"#), vec!["/api/users"]);
    }

    #[test]
    fn fetch_single_quoted() {
        assert_eq!(extract("fetch('/api/users')"), vec!["/api/users"]);
    }

    #[test]
    fn axios_get() {
        assert_eq!(extract(r#"axios.get("/users")"#), vec!["/users"]);
    }

    #[test]
    fn axios_post() {
        assert_eq!(extract(r#"axios.post('/orders')"#), vec!["/orders"]);
    }

    #[test]
    fn jquery_get() {
        assert_eq!(extract(r#"$.get("/legacy/x")"#), vec!["/legacy/x"]);
    }

    #[test]
    fn dedupe_across_patterns() {
        let src = r#"fetch("/x"); fetch('/x'); axios.get("/x");"#;
        assert_eq!(extract(src), vec!["/x"]);
    }

    #[test]
    fn multiple_distinct_urls_sorted() {
        let src = r#"fetch("/b"); axios.get("/a"); fetch("/c");"#;
        assert_eq!(extract(src), vec!["/a", "/b", "/c"]);
    }

    #[test]
    fn template_literal_with_interp_skipped() {
        // `${id}` makes the URL non-static — drop it.
        let src = "fetch(`/users/${id}`)";
        assert!(extract(src).is_empty());
    }

    #[test]
    fn substring_does_not_match() {
        // `mypreffetch(` should not match `fetch(`.
        let src = r#"prefetch("/x")"#;
        assert!(extract(src).is_empty());
    }
}
