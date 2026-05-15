//! Port of upstream `extractConsumerAccessedKeys`
//! (`gitnexus/src/core/ingestion/call-processor.ts:3199-3327`).
//!
//! Pure regex over file content. Detects three access patterns on
//! HTTP response variables and returns the union of accessed keys,
//! filtered by [`RESPONSE_ACCESS_BLOCKLIST`] to drop common JS API /
//! Array / Promise / DOM method names that would otherwise produce
//! false positives.
//!
//! Patterns (matches upstream exactly):
//! 1. Destructuring from `.json()` chain — `const {a,b} = await res.json()`
//!    also `const {a} = await (await fetch(...)).json()`
//! 2. Destructuring from a `data|result|response|json|body|res` variable
//!    — `const {a,b} = data`
//! 3. Property access on the same variable name list —
//!    `data.foo`, `response?.bar`, `result.baz`
//!
//! Returns a deduped, sorted `Vec<String>`. Empty when no patterns match.

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// Method/property names to drop when extracting Pattern 3 hits.
/// Covers Fetch API, Promise, Array, Object, DOM APIs that share names
/// with common response-variable identifiers. Verbatim port of upstream
/// `RESPONSE_ACCESS_BLOCKLIST`.
static RESPONSE_ACCESS_BLOCKLIST: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        // Fetch/Response API
        "json",
        "text",
        "blob",
        "arrayBuffer",
        "formData",
        "ok",
        "status",
        "headers",
        "clone",
        // Promise
        "then",
        "catch",
        "finally",
        // Array
        "map",
        "filter",
        "forEach",
        "reduce",
        "find",
        "some",
        "every",
        "push",
        "pop",
        "shift",
        "unshift",
        "splice",
        "slice",
        "concat",
        "join",
        "sort",
        "reverse",
        "includes",
        "indexOf",
        // Object
        "length",
        "toString",
        "valueOf",
        "keys",
        "values",
        "entries",
        // DOM methods — file-download patterns often reuse `data`/`response` variable names
        "appendChild",
        "removeChild",
        "insertBefore",
        "replaceChild",
        "replaceChildren",
        "createElement",
        "getElementById",
        "querySelector",
        "querySelectorAll",
        "setAttribute",
        "getAttribute",
        "removeAttribute",
        "hasAttribute",
        "addEventListener",
        "removeEventListener",
        "dispatchEvent",
        "classList",
        "className",
        "parentNode",
        "parentElement",
        "childNodes",
        "children",
        "nextSibling",
        "previousSibling",
        "firstChild",
        "lastChild",
        "click",
        "focus",
        "blur",
        "submit",
        "reset",
        "innerHTML",
        "outerHTML",
        "textContent",
        "innerText",
    ])
});

/// Pattern 1: `const { a, b } = await res.json()` and
/// `const { a } = await (await fetch(...)).json()` variants. Captures
/// the destructured body in group 1. The RHS accepts any chain that
/// ends in `.json()` from a known HTTP client identifier.
static JSON_CALL_DESTRUCTURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:const|let|var)\s+\{([^}]+)\}\s*=\s*(?:await\s+)?(?:\w+\.json\s*\(\)|(?:await\s+)?(?:fetch|axios|got)\s*\([^)]*\)(?:\.then\s*\([^)]*\))?(?:\.json\s*\(\))?)"
    ).expect("JSON_CALL_DESTRUCTURE regex compiles")
});

/// Pattern 2: `const { a, b } = data;` where the RHS is one of the
/// common response variable names. Captures the destructured body in
/// group 1. The `\b` word boundary keeps `dataset` from matching `data`.
static DATA_VAR_DESTRUCTURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:const|let|var)\s+\{([^}]+)\}\s*=\s*(?:data|result|response|json|body|res)\b")
        .expect("DATA_VAR_DESTRUCTURE regex compiles")
});

/// Inner pattern for splitting a destructured body into keys. Matches
/// `key` or `key: alias` and captures the **left** identifier (the
/// original property name on the source object, not the local alias).
static DESTRUCTURE_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\w+)\s*(?::\s*\w+)?").expect("DESTRUCTURE_KEY regex compiles"));

/// Pattern 3: `data.foo` / `response?.foo`. Anchors with `\b` so
/// `mydata.x` does not match. Captures the key name in group 1.
static PROP_ACCESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:data|response|result|json|body|res)\s*(?:\?\.|\.)(\w+)")
        .expect("PROP_ACCESS regex compiles")
});

/// Extract every response-key name a consumer file accesses, using
/// upstream's three regex passes. Output is deduped and sorted for
/// stable test output and reason-string canonicalisation.
pub fn extract(content: &str) -> Vec<String> {
    let mut keys: HashSet<String> = HashSet::new();

    for caps in JSON_CALL_DESTRUCTURE.captures_iter(content) {
        let body = &caps[1];
        for key_cap in DESTRUCTURE_KEY.captures_iter(body) {
            keys.insert(key_cap[1].to_string());
        }
    }

    for caps in DATA_VAR_DESTRUCTURE.captures_iter(content) {
        let body = &caps[1];
        for key_cap in DESTRUCTURE_KEY.captures_iter(body) {
            keys.insert(key_cap[1].to_string());
        }
    }

    for caps in PROP_ACCESS.captures_iter(content) {
        let key = &caps[1];
        if !RESPONSE_ACCESS_BLOCKLIST.contains(key) {
            keys.insert(key.to_string());
        }
    }

    let mut out: Vec<String> = keys.into_iter().collect();
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
    fn pattern1_json_destructure() {
        let src = "const { id, name } = await res.json()";
        assert_eq!(extract(src), vec!["id", "name"]);
    }

    #[test]
    fn pattern1_fetch_chain_json() {
        // Upstream regex matches `await fetch(...).json()` directly. It
        // does NOT match the doubly-wrapped `await (await fetch(...)).json()`
        // form despite the upstream comment's claim — `\w+\.json` cannot
        // see past the inner parens. We match upstream behavior exactly.
        let src = r#"const { items } = await fetch("/x").json()"#;
        assert_eq!(extract(src), vec!["items"]);
    }

    #[test]
    fn pattern2_data_destructure() {
        let src = "const { items, total } = data;";
        assert_eq!(extract(src), vec!["items", "total"]);
    }

    #[test]
    fn pattern2_response_destructure() {
        let src = "const { error } = response;";
        assert_eq!(extract(src), vec!["error"]);
    }

    #[test]
    fn pattern2_aliased_keeps_left() {
        let src = "const { id: userId } = data;";
        assert_eq!(extract(src), vec!["id"]);
    }

    #[test]
    fn pattern3_property_access() {
        let src = "let x = data.foo + response.bar;";
        assert_eq!(extract(src), vec!["bar", "foo"]);
    }

    #[test]
    fn pattern3_optional_chaining() {
        let src = "const y = data?.x;";
        assert_eq!(extract(src), vec!["x"]);
    }

    #[test]
    fn pattern3_blocklist_filters() {
        // `data.length` and `response.json` both hit the blocklist.
        let src = "if (data.length > 0) response.json();";
        assert_eq!(extract(src), Vec::<String>::new());
    }

    #[test]
    fn pattern3_mixed_kept_and_blocked() {
        let src = "data.items.forEach(); data.length;";
        // `items` kept; `length` blocked; `forEach` is on a child obj so
        // upstream's `data.X` regex does not capture it.
        assert_eq!(extract(src), vec!["items"]);
    }

    #[test]
    fn dedupe_across_patterns() {
        // `id` appears in destructure AND property access — count once.
        let src = "const { id } = data;\nconsole.log(data.id);";
        assert_eq!(extract(src), vec!["id"]);
    }

    #[test]
    fn word_boundary_excludes_lookalike_var() {
        // `mydata.field` must not match — `\b` prevents partial-name hits.
        let src = "mydata.field; databaseFoo.x;";
        assert!(extract(src).is_empty());
    }
}
