use crate::framework_helpers::strip_python_string_quotes;
use ecp_core::analyzer::types::RawRoute;

const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "options", "head", "connect", "trace",
    // `use` is an Express / Connect / Koa mount-point route — `app.use('/api', router)`
    // registers a path-prefixed sub-router that ref-gitnexus surfaces as a Route node.
    // The queries.scm matchers in JS / TS already include `use` in the verb allowlist;
    // adding it here lets `detect_from_call` finalize the RawRoute into a Route node
    // (previously dropped at builder.rs:445 — verified missing for 3 routes in
    // `examples/multi-router/index.js`, `examples/web-service/index.js` 2026-05-19).
    "use",
];

#[derive(Debug, Clone)]
pub struct DetectedRoute {
    pub method: String,
    pub path: String,
}

fn looks_like_path(s: &str) -> bool {
    // Strict: legitimate HTTP route literals start with `/`. The previous
    // lenient form (colon-, curly-, angle-, or pure-alphanumeric fallback)
    // produced a ~86% FP rate on the ecp self-corpus because
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

/// Thin shim over [`strip_python_string_quotes`] that preserves the `&str`
/// fallback contract expected by `detect_from_call`, `clean_route_path`, and
/// `clean_route_path_lax`. Now also handles triple-quote forms inherited from
/// the shared helper (previously this function only handled single/double).
fn strip_string_quotes(s: &str) -> &str {
    strip_python_string_quotes(s).unwrap_or(s)
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
/// registration sites. Bare paths (`'register'`, `'path/<x>/y'`) are
/// semantically equivalent to `/register` / `/path/<x>/y` per framework
/// convention — normalize by prepending `/` when missing, accept any
/// non-empty result.
///
/// The caller must have INDEPENDENT confidence the call is a route
/// registration (annotation name allowlist, attribute name allowlist,
/// member-method allowlist on a known router builder) — this helper
/// skips the `looks_like_path` FP filter that `clean_route_path`
/// enforces, so handing it `dict.get("key")` returns `Some("/key")`.
///
/// Frameworks confirmed in coverage:
///
/// | Lang     | Surface                                                      |
/// |----------|--------------------------------------------------------------|
/// | Python   | Flask `@app.route('register')`, Sanic `@app.route(...)`,     |
/// |          | FastAPI `add_api_route('p')`, Starlette `add_route(...)`     |
/// | PHP      | Laravel `Route::get('users', ...)`, Symfony `#[Route('p')]`  |
/// | TS / JS  | NestJS decorators `@Get('users')`, `@Post(':id')`            |
/// | Java     | Spring `@GetMapping("users")`, JAX-RS `@Path("users")`       |
/// | Kotlin   | Spring same as Java; Ktor `route("users")` DSL               |
/// | C#       | ASP.NET `[HttpGet("users")]`, `[Route("api")]`               |
///
/// The list above is intentionally `clean_route_path_lax` callers + the
/// 4 frameworks (TS NestJS, Java Spring, Kotlin Ktor, C# HttpGet) called
/// out as future emission sites in the 2026-05-17 route-precision spec.
/// Each lang's parser is responsible for the allowlist gate before
/// invoking this helper; see existing `python/parser.rs` and
/// `php/parser.rs` call sites for the established pattern.
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

    #[test]
    fn detect_from_call_accepts_express_use_mount_point() {
        // `app.use('/api', router)` is an Express/Connect mount-point route.
        // ref-gitnexus emits it as a Route node; ecp must too — without
        // `"use"` in HTTP_METHODS the call is silently dropped at
        // builder.rs:445, leaving the 3 routes in `examples/multi-router/`
        // and `examples/web-service/` unpaired in the parity report.
        let r = detect_from_call(&raw("use", "'/api/v1'")).unwrap();
        assert_eq!(r.method, "USE");
        assert_eq!(r.path, "/api/v1");
    }

    // -- lax helper: per-framework bare-path support -------------------------

    #[test]
    fn lax_strips_quotes_and_prepends_slash() {
        assert_eq!(
            clean_route_path_lax("'register'"),
            Some("/register".to_string())
        );
        assert_eq!(
            clean_route_path_lax("\"users\""),
            Some("/users".to_string())
        );
    }

    #[test]
    fn lax_preserves_leading_slash() {
        assert_eq!(clean_route_path_lax("'/users'"), Some("/users".to_string()));
        assert_eq!(
            clean_route_path_lax("\"/api/v1\""),
            Some("/api/v1".to_string())
        );
    }

    #[test]
    fn lax_handles_python_blueprint_shorthand() {
        // Flask `@bp.route('/block')` and `@app.route('users/<id>')` —
        // both shapes valid Flask bare paths.
        assert_eq!(
            clean_route_path_lax("'users/<id>'"),
            Some("/users/<id>".to_string())
        );
        assert_eq!(clean_route_path_lax("'/block'"), Some("/block".to_string()));
    }

    #[test]
    fn lax_handles_ts_nestjs_decorator_bare_path() {
        // NestJS `@Get('users')` / `@Post(':id')`.
        assert_eq!(clean_route_path_lax("'users'"), Some("/users".to_string()));
        assert_eq!(clean_route_path_lax("':id'"), Some("/:id".to_string()));
    }

    #[test]
    fn lax_handles_java_spring_request_mapping() {
        // Spring `@GetMapping("users")` / `@RequestMapping("api")`.
        // JAX-RS `@Path("users")` (Jakarta REST) has the same shape.
        assert_eq!(
            clean_route_path_lax("\"users\""),
            Some("/users".to_string())
        );
        assert_eq!(
            clean_route_path_lax("\"api/v1\""),
            Some("/api/v1".to_string())
        );
    }

    #[test]
    fn lax_handles_kotlin_ktor_route_dsl() {
        // Ktor `route("users") { get { ... } }`. Tree-sitter-kotlin
        // exposes the path via `string_content`, so the value reaching
        // this helper is already unquoted — exercise that path too.
        assert_eq!(clean_route_path_lax("users"), Some("/users".to_string()));
        assert_eq!(clean_route_path_lax("/api"), Some("/api".to_string()));
    }

    #[test]
    fn lax_handles_csharp_http_attribute() {
        // ASP.NET `[HttpGet("users")]`, `[Route("api/[controller]")]`.
        // `[controller]` is a template token that ref-gitnexus retains
        // verbatim; we shouldn't mangle the bracket contents.
        assert_eq!(
            clean_route_path_lax("\"users\""),
            Some("/users".to_string())
        );
        assert_eq!(
            clean_route_path_lax("\"api/[controller]\""),
            Some("/api/[controller]".to_string()),
        );
    }

    #[test]
    fn lax_rejects_empty_after_trim() {
        // `""`, `"   "`, and empty unquoted strings all yield `None` so
        // the caller doesn't emit a Route node with a bare `/`.
        assert_eq!(clean_route_path_lax("''"), None);
        assert_eq!(clean_route_path_lax("\"   \""), None);
        assert_eq!(clean_route_path_lax(""), None);
    }

    // -- strip_string_quotes shim: prefix-quote edge cases (FU-2026-05-23-026) --

    #[test]
    fn strip_quotes_bare_double() {
        assert_eq!(strip_string_quotes("\"/api\""), "/api");
    }

    #[test]
    fn strip_quotes_bare_single() {
        assert_eq!(strip_string_quotes("'/api'"), "/api");
    }

    #[test]
    fn strip_quotes_r_prefix() {
        assert_eq!(
            strip_string_quotes("r\"/path/to/<ext:file\\.(txt)>\""),
            "/path/to/<ext:file\\.(txt)>"
        );
    }

    #[test]
    fn strip_quotes_f_prefix() {
        assert_eq!(strip_string_quotes("f\"/api/{id}\""), "/api/{id}");
    }

    #[test]
    fn strip_quotes_rb_prefix() {
        assert_eq!(strip_string_quotes("rb\"/api\""), "/api");
    }

    #[test]
    fn strip_quotes_uppercase_prefix() {
        assert_eq!(strip_string_quotes("RB\"/api\""), "/api");
    }

    #[test]
    fn strip_quotes_triple_double() {
        assert_eq!(strip_string_quotes("\"\"\"/api\"\"\""), "/api");
    }

    #[test]
    fn strip_quotes_triple_single() {
        assert_eq!(strip_string_quotes("'''/api'''"), "/api");
    }

    #[test]
    fn strip_quotes_malformed_unmatched_returns_original() {
        // One closing quote missing — shim must fall back to original string.
        assert_eq!(strip_string_quotes("\"/api"), "\"/api");
    }

    #[test]
    fn strip_quotes_unquoted_path_returns_original() {
        // Already-unquoted path passes through unchanged.
        assert_eq!(strip_string_quotes("/api/users"), "/api/users");
    }
}
