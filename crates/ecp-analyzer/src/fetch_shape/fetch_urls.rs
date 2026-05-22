//! Extract static fetch URL literals from consumer source files.
//!
//! Companion to [`consumer_keys`](super::consumer_keys) — that module
//! pulls accessed keys; this one pulls the URLs whose responses are
//! being consumed. Together they let the builder emit
//! `RelType::Fetches` edges (consumer file → matching Route node).
//!
//! Patterns recognised:
//! - `fetch('/path')` / `fetch("/path")` (Fetch API, default GET)
//! - `fetch('/path', { method: 'POST' })` (Fetch API with explicit method)
//! - `axios.get|post|put|delete|patch('/path')` (axios)
//! - `$.get|post('/path')` (jQuery shorthand)
//! - `requests.get|post|...('/path')` (Python requests)
//! - `httpx.get|post|...('/path')` (Python httpx)
//!
//! Returns `(http_method, url)` pairs — method always uppercased. The caller
//! (`builder.rs` Pass 1.6b) applies path normalization before matching.

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// `fetch("…")` / `fetch('…')` — first arg only, no `${…}` interpolation.
/// Method defaults to GET; overridden by `FETCH_WITH_METHOD` when a second
/// argument with `method:` is present.
static FETCH_BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bfetch\s*\(\s*['"`]([^'"`$\s]+)['"`]"#).expect("FETCH_BARE"));

/// `fetch(url, { method: 'POST' })` — captures url and explicit method.
/// `"method"` key with double-quotes also matched.
static FETCH_WITH_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\bfetch\s*\(\s*['"`]([^'"`$\s]+)['"`]\s*,\s*\{[^}]*["']method["']\s*:\s*['"]([A-Za-z]+)['"]"#,
    )
    .expect("FETCH_WITH_METHOD")
});

/// `axios.METHOD('/path')` — METHOD is one of get|post|put|delete|patch.
static AXIOS_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\baxios\s*\.\s*(get|post|put|delete|patch)\s*\(\s*['"`]([^'"`$\s]+)['"`]"#)
        .expect("AXIOS_CALL")
});

/// `$.get('/x')` / `$.post('/x')` — bare-URL jQuery shorthand.
static JQUERY_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\$\s*\.\s*(get|post)\s*\(\s*['"`]([^'"`$\s]+)['"`]"#).expect("JQUERY_CALL")
});

/// Python `requests.METHOD('/path')` or `httpx.METHOD('/path')`.
static PYTHON_HTTP_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\b(?:requests|httpx)\s*\.\s*(get|post|put|delete|patch|head|options)\s*\(\s*['"`]([^'"`$\s]+)['"`]"#,
    )
    .expect("PYTHON_HTTP_CALL")
});

/// Extract every statically-resolvable `(http_method, url)` pair from `content`.
///
/// `http_method` is always uppercased. Output is deduped and sorted by
/// `(url, method)` for stable downstream behaviour.
pub fn extract(content: &str) -> Vec<(String, String)> {
    let mut pairs: HashSet<(String, String)> = HashSet::new();

    // Explicit-method fetch() must be inserted first so the bare-GET fallback
    // below can detect collisions on the same URL and skip the default-GET entry.
    for caps in FETCH_WITH_METHOD.captures_iter(content) {
        pairs.insert((caps[2].to_uppercase(), caps[1].to_string()));
    }

    // Bare fetch() — default GET; skip URLs already captured with an explicit method.
    for caps in FETCH_BARE.captures_iter(content) {
        let url = caps[1].to_string();
        if !pairs.iter().any(|(_, u)| u == &url) {
            pairs.insert(("GET".to_string(), url));
        }
    }

    for caps in AXIOS_CALL.captures_iter(content) {
        pairs.insert((caps[1].to_uppercase(), caps[2].to_string()));
    }
    for caps in JQUERY_CALL.captures_iter(content) {
        pairs.insert((caps[1].to_uppercase(), caps[2].to_string()));
    }
    for caps in PYTHON_HTTP_CALL.captures_iter(content) {
        pairs.insert((caps[1].to_uppercase(), caps[2].to_string()));
    }

    let mut out: Vec<(String, String)> = pairs.into_iter().collect();
    out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
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
    fn fetch_bare_defaults_to_get() {
        assert_eq!(
            extract(r#"fetch("/api/users")"#),
            vec![("GET".to_string(), "/api/users".to_string())]
        );
    }

    #[test]
    fn fetch_single_quoted() {
        assert_eq!(
            extract("fetch('/api/users')"),
            vec![("GET".to_string(), "/api/users".to_string())]
        );
    }

    #[test]
    fn fetch_with_post_method() {
        assert_eq!(
            extract(r#"fetch('/api/users', { 'method': 'POST' })"#),
            vec![("POST".to_string(), "/api/users".to_string())]
        );
    }

    #[test]
    fn fetch_with_double_quoted_method_key() {
        let pairs = extract(r#"fetch('/api/items', { "method": "PUT" })"#);
        assert!(
            pairs.contains(&("PUT".to_string(), "/api/items".to_string())),
            "expected PUT /api/items in {pairs:?}"
        );
    }

    #[test]
    fn axios_get() {
        assert_eq!(
            extract(r#"axios.get("/users")"#),
            vec![("GET".to_string(), "/users".to_string())]
        );
    }

    #[test]
    fn axios_post() {
        assert_eq!(
            extract(r#"axios.post('/orders')"#),
            vec![("POST".to_string(), "/orders".to_string())]
        );
    }

    #[test]
    fn jquery_get() {
        assert_eq!(
            extract(r#"$.get("/legacy/x")"#),
            vec![("GET".to_string(), "/legacy/x".to_string())]
        );
    }

    #[test]
    fn python_requests_get() {
        assert_eq!(
            extract(r#"requests.get('/api/users')"#),
            vec![("GET".to_string(), "/api/users".to_string())]
        );
    }

    #[test]
    fn python_httpx_post() {
        assert_eq!(
            extract(r#"httpx.post('/api/items')"#),
            vec![("POST".to_string(), "/api/items".to_string())]
        );
    }

    #[test]
    fn dedupe_same_method_url() {
        let src = r#"fetch("/x"); fetch('/x'); axios.get("/x");"#;
        assert_eq!(extract(src), vec![("GET".to_string(), "/x".to_string())]);
    }

    #[test]
    fn multiple_distinct_sorted() {
        let src = r#"fetch("/b"); axios.get("/a"); fetch("/c");"#;
        assert_eq!(
            extract(src),
            vec![
                ("GET".to_string(), "/a".to_string()),
                ("GET".to_string(), "/b".to_string()),
                ("GET".to_string(), "/c".to_string()),
            ]
        );
    }

    #[test]
    fn template_literal_with_interp_skipped() {
        // `${id}` makes URL non-static — drop it.
        let src = "fetch(`/users/${id}`)";
        assert!(extract(src).is_empty());
    }

    #[test]
    fn substring_does_not_match_fetch() {
        let src = r#"prefetch("/x")"#;
        assert!(extract(src).is_empty());
    }
}
