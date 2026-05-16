use graph_nexus_core::analyzer::types::RawRoute;

const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "options", "head", "connect", "trace",
];

#[derive(Debug, Clone)]
pub struct DetectedRoute {
    pub method: String,
    pub path: String,
}

fn looks_like_path(s: &str) -> bool {
    // Strict: legitimate HTTP route literals start with `/`. The previous
    // lenient form (colon-, curly-, angle-, or pure-alphanumeric fallback)
    // produced a ~86% FP rate on the gnx-rs self-corpus because
    // `dict.get("key")` / `Map.get(...)` / `headers.get(...)` all matched.
    // Frameworks whose canonical literal is bare (`[HttpGet("users")]` in
    // C#) need their own parser-side path — they should not piggy-back on
    // this generic predicate. Spec: 2026-05-17-route-precision-design.md.
    s.starts_with('/')
}

fn extract_string_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let quote = c;
            let mut arg = String::new();
            while let Some(&next_c) = chars.peek() {
                if next_c == quote {
                    chars.next(); // consume closing quote
                    break;
                }
                arg.push(next_c);
                chars.next();
            }
            args.push(arg);
        }
    }
    args
}

pub fn detect_from_decorator(decorator: &str) -> Option<DetectedRoute> {
    let lower = decorator.to_lowercase();
    let method = HTTP_METHODS.iter().find(|&&m| lower.contains(m))?;

    let string_args = extract_string_args(decorator);
    let path = string_args.iter().find(|s| looks_like_path(s))?;

    Some(DetectedRoute {
        method: method.to_uppercase(),
        path: path.clone(),
    })
}

pub fn detect_from_call(raw: &RawRoute) -> Option<DetectedRoute> {
    let lower = raw.method.to_lowercase();
    let method = HTTP_METHODS.iter().find(|&&m| lower.contains(m))?;

    // Raw path may arrive wrapped in `"…"` / `'…'` because Python / TS
    // tree-sitter `string` nodes carry the literal quote bytes. Peel them
    // so downstream Route nodes get names like `GET /api/users`, not
    // `GET "/api/users"`. Fall back to the raw text if it isn't quoted.
    let stripped = strip_string_quotes(&raw.path);
    if looks_like_path(stripped) {
        Some(DetectedRoute {
            method: method.to_uppercase(),
            path: stripped.to_string(),
        })
    } else {
        None
    }
}

/// Trim matching surrounding single / double quotes from a string literal
/// captured as raw source text. Returns the inner slice when both ends
/// match, otherwise the original string.
///
/// Also strips Python string-prefix sigils (`r`, `b`, `f`, `u`, `rb`, `br`,
/// case-insensitive) so paths like `r"/path/to/<ext:file\.(txt)>"`
/// (raw-string regex routes common in Sanic) and `b"/x"` (byte-string)
/// reach `looks_like_path` as `/path/to/<ext:file\.(txt)>` and `/x`.
fn strip_string_quotes(s: &str) -> &str {
    // Try direct quote-stripping first; fall through to prefix-aware path.
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    // Strip up to 2 prefix bytes (e.g. `r`, `rb`, `br`), then re-check.
    // Prefix bytes must be ASCII for split_at to be UTF-8-safe — paths
    // starting with multibyte chars (`/啊`) would otherwise panic at the
    // byte boundary.
    for prefix_len in [2, 1] {
        if s.len() < prefix_len + 2 || !s.is_char_boundary(prefix_len) {
            continue;
        }
        let (prefix, rest) = s.split_at(prefix_len);
        if !prefix
            .bytes()
            .all(|b| matches!(b, b'r' | b'R' | b'b' | b'B' | b'f' | b'F' | b'u' | b'U'))
        {
            continue;
        }
        for q in ['"', '\''] {
            if rest.starts_with(q) && rest.ends_with(q) && rest.len() >= 2 {
                return &rest[1..rest.len() - 1];
            }
        }
    }
    s
}

/// Parser-side helper: strip surrounding quotes from a tree-sitter string
/// capture and return the path only if it satisfies the strict route
/// shape check. Used to keep `RawRoute` records clean of `dict.get("key")`
/// style FPs at extraction time, instead of relying on a downstream filter
/// in the builder. Returns `None` when the literal is not a route path.
pub fn clean_route_path(raw_with_quotes: &str) -> Option<String> {
    let stripped = strip_string_quotes(raw_with_quotes);
    looks_like_path(stripped).then(|| stripped.to_string())
}

/// Relaxed variant of `clean_route_path` for framework-specific route-
/// registration methods (Flask `@app.route(...)`, Sanic `@app.route(...)`,
/// FastAPI `add_api_route(...)`, Laravel `Route::get(...)`). These accept
/// bare paths (`'register'`, `'path/<x>/y'`) per framework convention —
/// semantically equivalent to `/register` / `/path/<x>/y`. Normalizes by
/// prepending `/` when missing and accepts any non-empty result. The caller
/// should only invoke this when it has independent confidence the call is
/// a route registration (method-name allowlist), since this skips the
/// `looks_like_path` FP filter that `clean_route_path` enforces.
pub fn clean_route_path_lax(raw_with_quotes: &str) -> Option<String> {
    let stripped = strip_string_quotes(raw_with_quotes).trim();
    if stripped.is_empty() {
        return None;
    }
    if stripped.starts_with('/') {
        Some(stripped.to_string())
    } else {
        Some(format!("/{stripped}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(method: &str, path: &str) -> RawRoute {
        RawRoute {
            method: method.to_string(),
            path: path.to_string(),
            handler: None,
            span: (0, 0, 0, 0),
        }
    }

    #[test]
    fn detect_from_call_strips_double_quoted_path() {
        let r = detect_from_call(&raw("get", "\"/api/users\"")).unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/api/users");
    }

    #[test]
    fn detect_from_call_strips_single_quoted_path() {
        let r = detect_from_call(&raw("post", "'/users/:id'")).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/users/:id");
    }

    #[test]
    fn detect_from_call_preserves_unquoted_path() {
        let r = detect_from_call(&raw("delete", "/items/{id}")).unwrap();
        assert_eq!(r.method, "DELETE");
        assert_eq!(r.path, "/items/{id}");
    }

    #[test]
    fn detect_from_call_rejects_non_path_string() {
        assert!(detect_from_call(&raw("get", "not_a_path!")).is_none());
    }
}
