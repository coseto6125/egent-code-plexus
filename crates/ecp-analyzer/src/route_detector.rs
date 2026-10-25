use crate::framework_helpers::strip_python_string_quotes;
use ecp_core::analyzer::types::RawRoute;
use ecp_core::contracts::raw_records::{RawEndpoint, Unbound};

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

/// The verbs a Laravel `Route::any` route answers: `Router::$verbs`.
const ANY_ROUTE_VERBS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Whether an `ANY` route answers a request of `method`, in any case.
pub fn any_route_answers(method: &str) -> bool {
    ANY_ROUTE_VERBS
        .iter()
        .any(|verb| verb.eq_ignore_ascii_case(method))
}

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

/// The text after `method =` / `methods:` in a decorator's argument list.
/// The scan skips quoted spans, so a path such as `"/method=get"` never
/// matches, and requires the key to start an argument (after `(` or `,`), so
/// `RequestMethod.method` cannot match either.
fn method_argument(decorator: &str) -> Option<&str> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut prev = None;
    let mut chars = decorator.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if let Some(q) = quote {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, _) if c == q => quote = None,
                _ => {}
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c.is_alphabetic() || c == '_' {
            let mut end = start + c.len_utf8();
            while let Some(&(i, next)) = chars.peek() {
                if !(next.is_alphanumeric() || next == '_') {
                    break;
                }
                end = i + next.len_utf8();
                chars.next();
            }
            let key = &decorator[start..end];
            if matches!(prev, Some('(' | ','))
                && (key.eq_ignore_ascii_case("method") || key.eq_ignore_ascii_case("methods"))
            {
                let rest = decorator[end..].trim_start();
                if let Some(value) = rest.strip_prefix(['=', ':']) {
                    return Some(value);
                }
            }
            prev = Some('a');
            continue;
        }
        if !c.is_whitespace() {
            prev = Some(c);
        }
    }
    None
}

/// The verbs in one `method` argument value, in source order: identifiers
/// (`RequestMethod.GET`, `HttpMethod.Post`) and quoted strings (`'GET'`),
/// alone or inside `[..]` / `{..}`. The value ends at the first `,` or
/// unmatched `)` outside brackets.
fn verbs_in_method_value(value: &str) -> Vec<&'static str> {
    fn flush(token: &mut String, verbs: &mut Vec<&'static str>) {
        let name = token.rsplit('.').next().unwrap_or("").to_lowercase();
        token.clear();
        if let Some(verb) = HTTP_METHODS.iter().copied().find(|&m| m == name) {
            if !verbs.contains(&verb) {
                verbs.push(verb);
            }
        }
    }

    let mut verbs = Vec::new();
    let mut token = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in value.chars() {
        if let Some(q) = quote {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, _) if c == q => {
                    quote = None;
                    flush(&mut token, &mut verbs);
                }
                _ => token.push(c),
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                flush(&mut token, &mut verbs);
                quote = Some(c);
            }
            '[' | '{' | '(' => {
                flush(&mut token, &mut verbs);
                depth += 1;
            }
            ']' | '}' | ')' => {
                flush(&mut token, &mut verbs);
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ',' => {
                flush(&mut token, &mut verbs);
                if depth == 0 {
                    break;
                }
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' => token.push(c),
            _ => flush(&mut token, &mut verbs),
        }
    }
    flush(&mut token, &mut verbs);
    verbs
}

/// The HTTP verbs a decorator names, in source order. A verb-named decorator
/// (`@GetMapping`, `[HttpPost]`, `@router.delete`) reads it from the name token
/// only: the path and keyword arguments may contain verb substrings
/// (`@PostMapping("/get-token")`). A generic route decorator (`@RequestMapping`,
/// Flask `@app.route`, Symfony `#[Route]`) reads it from its `method` /
/// `methods` argument, outside string literals; without one it names no verb,
/// so a class-level `@RequestMapping("/api")` yields none.
fn decorator_verbs(decorator: &str) -> Vec<&'static str> {
    let trimmed = decorator
        .trim()
        .trim_start_matches(['#', '@', '['])
        .trim_start();
    let name_end = trimmed
        .find(|c: char| c == '(' || c == ']' || c.is_whitespace())
        .unwrap_or(trimmed.len());
    let name = trimmed[..name_end]
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();
    if matches!(name.as_str(), "requestmapping" | "route" | "api_route") {
        return method_argument(trimmed)
            .map(verbs_in_method_value)
            .unwrap_or_default();
    }
    let verb = name.strip_prefix("http").unwrap_or(&name);
    let verb = verb.strip_suffix("mapping").unwrap_or(verb);
    HTTP_METHODS
        .iter()
        .copied()
        .find(|&m| m == verb)
        .into_iter()
        .collect()
}

/// One route per verb a decorator names (`method = {GET, POST}` is two).
pub fn detect_all_from_decorator(decorator: &str) -> Vec<DetectedRoute> {
    let verbs = decorator_verbs(decorator);
    if verbs.is_empty() {
        return Vec::new();
    }
    let Some(path) = extract_string_args(decorator)
        .into_iter()
        .find(|s| looks_like_path(s))
    else {
        return Vec::new();
    };
    verbs
        .into_iter()
        .map(|verb| DetectedRoute {
            method: verb.to_uppercase(),
            path: path.clone(),
        })
        .collect()
}

/// The first route [`detect_all_from_decorator`] finds. Both callers
/// (`builder.rs` pass 1.5, `entry_points.rs`) take one route per decorator.
pub fn detect_from_decorator(decorator: &str) -> Option<DetectedRoute> {
    detect_all_from_decorator(decorator).into_iter().next()
}

pub fn detect_from_call(raw: &RawRoute) -> Option<DetectedRoute> {
    let lower = raw.method.to_lowercase();

    // gRPC fast path: the protobuf provider emits already-normalized records
    // (method `GRPC`, path `/<package.>Service/Method`) from `service { rpc }`
    // blocks — these are confirmed service endpoints, not literals to be
    // filtered, so they bypass the HTTP-method allowlist below (which exists
    // to reject non-route call sites that happen to carry a path-like string).
    if lower == "grpc" && raw.path.starts_with('/') {
        return Some(DetectedRoute {
            method: "GRPC".to_string(),
            path: raw.path.clone(),
        });
    }

    // Mount fast path, same shape as gRPC above: the Rails routes walker
    // emits `mount Engine, at: "/jobs"` as a normalized record (method
    // `MOUNT`, leading-slash path). A mount point is where an engine attaches
    // its own route set, so it belongs in a route inventory, and `mount`
    // matches no entry in the HTTP-method allowlist below. No other provider
    // emits this method — JS `app.mount(…)` never reaches here, because the
    // JS matcher captures only the verbs in its own allowlist.
    if lower == "mount" && raw.path.starts_with('/') {
        return Some(DetectedRoute {
            method: "MOUNT".to_string(),
            path: raw.path.clone(),
        });
    }

    // `ANY` fast path, same shape as mount: Laravel `Route::any('/x', ...)`
    // answers every verb, and the PHP parser emits it as one normalized
    // record that the allowlist below would reject.
    if lower == "any" && raw.path.starts_with('/') {
        return Some(DetectedRoute {
            method: "ANY".to_string(),
            path: raw.path.clone(),
        });
    }

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

/// Route path segments joined the way a router with groups joins a group
/// prefix to a route path (Laravel `prefix`): each segment trimmed of `/`,
/// empty segments dropped, one leading `/`. `["admin/", "/users"]` gives
/// `/admin/users`; no non-empty segment gives `/`.
pub fn join_route_path<'a>(segments: impl IntoIterator<Item = &'a str>) -> String {
    let mut path = String::new();
    for part in segments
        .into_iter()
        .flat_map(|segment| segment.split('/'))
        .filter(|part| !part.is_empty())
    {
        path.push('/');
        path.push_str(part);
    }
    if path.is_empty() {
        path.push('/');
    }
    path
}

/// BlindSpot kind of a route whose registration names a handler that binds
/// no declaration of its file: a Rails `posts#index`, an imported Express
/// handler, two same-name candidates. Its hint is [`unbound_handler_hint`].
pub const BS_KIND_ROUTE_UNBOUND_HANDLER: &str = "route-unbound-handler";

/// BlindSpot kind of a route whose registration shows no handler that the
/// parser reads: a gRPC `rpc`, a handler that a call builds. Its hint is
/// [`absent_handler_hint`].
pub const BS_KIND_ROUTE_HANDLER_ABSENT: &str = "route-handler-absent";

const UNBOUND_HANDLER_HEAD: &str = " names handler `";
const UNBOUND_HANDLER_TAIL: &str = "`, which binds no declaration of this file";
const ABSENT_HANDLER_TAIL: &str = ": the registration shows no handler the parser reads";

/// The hint of a [`BS_KIND_ROUTE_UNBOUND_HANDLER`] BlindSpot. It starts
/// with the Route node's name, because one registration can register
/// several routes at one site.
pub fn unbound_handler_hint(route_name: &str, handler: &str) -> String {
    format!("{route_name}{UNBOUND_HANDLER_HEAD}{handler}{UNBOUND_HANDLER_TAIL}")
}

/// The handler name in an [`unbound_handler_hint`] of `route_name`; `None`
/// for a hint of another route.
pub fn unbound_handler<'h>(hint: &'h str, route_name: &str) -> Option<&'h str> {
    hint.strip_prefix(route_name)?
        .strip_prefix(UNBOUND_HANDLER_HEAD)?
        .strip_suffix(UNBOUND_HANDLER_TAIL)
}

/// The hint of a [`BS_KIND_ROUTE_HANDLER_ABSENT`] BlindSpot.
pub fn absent_handler_hint(route_name: &str) -> String {
    format!("{route_name}{ABSENT_HANDLER_TAIL}")
}

/// Whether `hint` is the [`absent_handler_hint`] of `route_name`.
pub fn is_absent_handler_hint(hint: &str, route_name: &str) -> bool {
    hint.strip_prefix(route_name) == Some(ABSENT_HANDLER_TAIL)
}

/// The handler that a registration call passes as its last argument
/// (Express, gin and their kin run the last one): a function written in
/// place is anonymous, an argument of a kind in `name_kinds` names the
/// handler by its text, and anything else (the path alone, a call that
/// builds the handler) is no handler. Comments are skipped.
pub fn last_argument_handler(
    args: Option<tree_sitter::Node>,
    source: &[u8],
    function_kinds: &[&str],
    name_kinds: &[&str],
) -> Option<RawEndpoint> {
    let args = args?;
    let mut cursor = args.walk();
    let last = args
        .named_children(&mut cursor)
        .filter(|n| !n.is_extra())
        .last()?;
    match last.kind() {
        kind if function_kinds.contains(&kind) => Some(RawEndpoint::Unresolved(Unbound::Anonymous)),
        kind if name_kinds.contains(&kind) => RawRoute::named_handler(last.utf8_text(source).ok()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unbound_handler_hint_round_trip_returns_name_for_own_route_only() {
        let hint = unbound_handler_hint("GET /users", "posts#index");
        assert_eq!(unbound_handler(&hint, "GET /users"), Some("posts#index"));
        assert_eq!(unbound_handler(&hint, "POST /users"), None);
        assert_eq!(unbound_handler(&hint, "GET /user"), None);
        assert_eq!(unbound_handler("", "GET /users"), None);
    }

    #[test]
    fn test_is_absent_handler_hint_own_route_true_other_false() {
        let hint = absent_handler_hint("GRPC /pkg.Svc/M");
        assert!(is_absent_handler_hint(&hint, "GRPC /pkg.Svc/M"));
        assert!(!is_absent_handler_hint(&hint, "GRPC /pkg.Svc"));
        assert!(!is_absent_handler_hint("", "GRPC /pkg.Svc/M"));
    }

    fn raw(method: &str, path: &str) -> RawRoute {
        RawRoute {
            method: method.to_string(),
            path: path.to_string(),
            handler: None,
            span: (0, 0, 0, 0),
            scope: None,
        }
    }

    #[test]
    fn test_any_route_answers_laravel_verb_any_case_true() {
        assert!(any_route_answers("get"));
        assert!(any_route_answers("OPTIONS"));
    }

    #[test]
    fn test_any_route_answers_verb_any_never_registers_false() {
        assert!(!any_route_answers("TRACE"));
        assert!(!any_route_answers("CONNECT"));
        assert!(!any_route_answers(""));
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
    fn test_detect_from_call_any_with_path_keeps_any_method() {
        let detected = detect_from_call(&raw("ANY", "/webhook")).unwrap();
        assert_eq!(
            (detected.method.as_str(), detected.path.as_str()),
            ("ANY", "/webhook")
        );
    }

    #[test]
    fn test_detect_from_call_any_without_leading_slash_returns_none() {
        assert!(detect_from_call(&raw("ANY", "webhook")).is_none());
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

    #[test]
    fn detect_from_call_accepts_grpc_service_method() {
        // The protobuf provider emits `GRPC` / `/pkg.Service/Method` from a
        // `service { rpc }` block. `grpc` is not an HTTP method, so without
        // the gRPC fast path the route is dropped at builder.rs and the
        // service contract never reaches the graph.
        let r = detect_from_call(&raw("GRPC", "/helloworld.Greeter/SayHello")).unwrap();
        assert_eq!(r.method, "GRPC");
        assert_eq!(r.path, "/helloworld.Greeter/SayHello");
    }

    #[test]
    fn detect_from_call_accepts_rails_engine_mount() {
        // The Rails routes walker emits mount points as method `MOUNT`;
        // `mount` matches no HTTP-method allowlist entry, so without the
        // fast path the engine's attachment point leaves the inventory.
        let r = detect_from_call(&raw("MOUNT", "/jobs")).unwrap();
        assert_eq!(r.method, "MOUNT");
        assert_eq!(r.path, "/jobs");
    }

    #[test]
    fn detect_from_call_mount_requires_leading_slash() {
        assert!(detect_from_call(&raw("MOUNT", "jobs")).is_none());
    }

    #[test]
    fn detect_from_call_grpc_requires_leading_slash() {
        // A malformed gRPC path without the wire-format leading slash is not a
        // valid endpoint — reject rather than emit a junk Route.
        assert!(detect_from_call(&raw("GRPC", "Greeter/SayHello")).is_none());
    }

    // -- decorator verb comes from the name token, not the whole text --------

    fn dec(d: &str) -> Option<(String, String)> {
        detect_from_decorator(d).map(|r| (r.method, r.path))
    }

    fn some(m: &str, p: &str) -> Option<(String, String)> {
        Some((m.to_string(), p.to_string()))
    }

    #[test]
    fn detect_from_decorator_spring_post_mapping_path_has_get_substring() {
        assert_eq!(
            dec("@PostMapping(\"/get-token\")"),
            some("POST", "/get-token")
        );
        assert_eq!(dec("@GetMapping(\"/post-it\")"), some("GET", "/post-it"));
    }

    #[test]
    fn detect_from_decorator_class_level_request_mapping_yields_none() {
        assert_eq!(dec("@RequestMapping(\"/api/target\")"), None);
        assert_eq!(dec("@RequestMapping(value = \"/api/target\")"), None);
    }

    #[test]
    fn detect_from_decorator_request_mapping_with_method_uses_request_method() {
        assert_eq!(
            dec("@RequestMapping(value = \"/x/get\", method = RequestMethod.POST)"),
            some("POST", "/x/get")
        );
    }

    #[test]
    fn detect_from_decorator_kotlin_nest_python_csharp_use_name_token() {
        assert_eq!(dec("@PutMapping(\"/budget\")"), some("PUT", "/budget"));
        assert_eq!(dec("@Post('/get-token')"), some("POST", "/get-token"));
        assert_eq!(
            dec("@router.delete(\"/target\")"),
            some("DELETE", "/target")
        );
        assert_eq!(
            dec("[HttpPost(\"/get-token\")]"),
            some("POST", "/get-token")
        );
        assert_eq!(dec("[Route(\"/api/get\")]"), None);
    }

    #[test]
    fn detect_from_decorator_empty_or_whitespace_yields_none() {
        assert_eq!(dec(""), None);
        assert_eq!(dec("   "), None);
        assert_eq!(dec("@"), None);
    }

    #[test]
    fn detect_from_decorator_non_route_decorator_with_verb_in_path_yields_none() {
        assert_eq!(dec("@Autowired(\"/get\")"), None);
    }

    // -- one case per decorator style this function serves ---------------------
    //
    // `detect_from_decorator` serves Java/Kotlin Spring, C# attributes, PHP
    // attributes, TypeScript NestJS and Python decorators (the callers read
    // `RawNode.decorators`). Go, Rust, Ruby, Swift, C, C++ and Dart route
    // detection goes through `detect_from_call`, not this function.

    #[test]
    fn test_detect_from_decorator_spring_request_mapping_verb_word_in_path_keeps_method() {
        assert_eq!(
            dec("@RequestMapping(value=\"/RequestMethod.GET\", method=RequestMethod.POST)"),
            some("POST", "/RequestMethod.GET")
        );
        assert_eq!(
            dec("@RequestMapping(method = RequestMethod.post, value = \"/post\")"),
            some("POST", "/post")
        );
    }

    #[test]
    fn test_detect_from_decorator_spring_method_array_yields_one_route_per_verb() {
        let routes = detect_all_from_decorator(
            "@RequestMapping(value = \"/x\", method = {RequestMethod.GET, RequestMethod.POST})",
        );
        let got: Vec<_> = routes
            .iter()
            .map(|r| (r.method.as_str(), r.path.as_str()))
            .collect();
        assert_eq!(got, [("GET", "/x"), ("POST", "/x")]);
        assert_eq!(
            dec(
                "@RequestMapping(value = \"/x\", method = {RequestMethod.GET, RequestMethod.POST})"
            ),
            some("GET", "/x")
        );
    }

    #[test]
    fn test_detect_from_decorator_kotlin_method_array_and_get_mapping_verb_word_in_path() {
        assert_eq!(
            dec("@RequestMapping(\"/x\", method = [RequestMethod.DELETE])"),
            some("DELETE", "/x")
        );
        assert_eq!(
            dec("@GetMapping(\"/get-token\")"),
            some("GET", "/get-token")
        );
        assert_eq!(
            dec("@GetMapping(value = [\"/post\"])"),
            some("GET", "/post")
        );
    }

    #[test]
    fn test_detect_from_decorator_csharp_http_attribute_verb_word_in_path_keeps_method() {
        assert_eq!(dec("[HttpGet(\"/x\")]"), some("GET", "/x"));
        assert_eq!(dec("[HttpDelete(\"/post\")]"), some("DELETE", "/post"));
        assert_eq!(dec("[HttpGet(\"/get-token\")]"), some("GET", "/get-token"));
    }

    #[test]
    fn test_detect_from_decorator_php_symfony_attribute_reads_methods_argument() {
        assert_eq!(dec("#[Route('/x', methods: ['GET'])]"), some("GET", "/x"));
        assert_eq!(dec("#[Route(\"/x\", methods: \"PUT\")]"), some("PUT", "/x"));
        assert_eq!(
            dec("#[Route('/x', name: 'x', methods: ['post', 'GET'])]"),
            some("POST", "/x")
        );
        let verbs: Vec<_> = detect_all_from_decorator("#[Route('/x', methods: ['GET', 'HEAD'])]")
            .into_iter()
            .map(|r| r.method)
            .collect();
        assert_eq!(verbs, ["GET", "HEAD"]);
    }

    #[test]
    fn test_detect_from_decorator_php_symfony_attribute_verb_word_in_path_or_empty_methods() {
        assert_eq!(
            dec("#[Route('/get-token', methods: ['POST'])]"),
            some("POST", "/get-token")
        );
        assert_eq!(
            dec("#[Route('/post', methods: ['DELETE'])]"),
            some("DELETE", "/post")
        );
        assert_eq!(dec("#[Route('/get-token')]"), None);
        assert_eq!(dec("#[Route('/get-token', methods: [])]"), None);
        assert_eq!(dec("#[Route('/methods: GET')]"), None);
    }

    #[test]
    fn test_detect_from_decorator_nestjs_verb_word_in_path_keeps_method() {
        assert_eq!(dec("@Get('/x')"), some("GET", "/x"));
        assert_eq!(dec("@Get('/post')"), some("GET", "/post"));
        assert_eq!(dec("@Post('/get-token')"), some("POST", "/get-token"));
    }

    #[test]
    fn test_detect_from_decorator_python_shorthand_and_route_methods() {
        assert_eq!(dec("@app.get(\"/x\")"), some("GET", "/x"));
        assert_eq!(dec("@app.get(\"/post\")"), some("GET", "/post"));
        assert_eq!(
            dec("@app.route(\"/x\", methods=[\"POST\"])"),
            some("POST", "/x")
        );
        assert_eq!(
            dec("@app.route('/get-token', methods=['PUT'])"),
            some("PUT", "/get-token")
        );
        assert_eq!(dec("@app.route(\"/get-token\")"), None);
        assert_eq!(dec("@app.route(\"/x\", methods=[])"), None);
    }

    #[test]
    fn test_detect_from_decorator_escaped_quote_and_bare_name_edges() {
        // Verb only: path extraction does not honour escapes, and this test
        // must not freeze that.
        assert_eq!(
            decorator_verbs(
                "@RequestMapping(value=\"/a\\\"method=GET\", method=RequestMethod.PUT)"
            ),
            ["put"]
        );
        assert_eq!(dec("@RequestMapping"), None);
        assert_eq!(dec("#[Route]"), None);
        assert_eq!(dec("@RequestMapping(method)"), None);
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

    #[test]
    fn test_join_route_path_stray_slashes_joins_with_single_slashes() {
        assert_eq!(
            join_route_path(["/admin/", "import", "/award/"]),
            "/admin/import/award"
        );
    }

    #[test]
    fn test_join_route_path_empty_segments_returns_root() {
        assert_eq!(join_route_path(["", "/", ""]), "/");
    }

    #[test]
    fn test_join_route_path_empty_prefix_keeps_path() {
        assert_eq!(join_route_path(["", "/users/{id}"]), "/users/{id}");
    }
}
