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
    s.starts_with('/')
        || s.contains(':')
        || s.contains('{')
        || s.contains('<')
        // Lenient fallback for cases like [HttpGet("users")]
        || (!s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'))
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
fn strip_string_quotes(s: &str) -> &str {
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
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
