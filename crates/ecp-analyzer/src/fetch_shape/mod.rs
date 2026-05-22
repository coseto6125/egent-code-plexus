//! HTTP fetch / response shape extraction.
//!
//! Ports upstream gitnexus' regex-based extractors into Rust so the
//! graph builder can populate `RelType::Fetches` edges (consumer →
//! Route) and per-Route response-shape metadata. Both extractors are
//! pure functions over file content — they hold no graph references
//! and can be unit-tested in isolation.
//!
//! Pipeline:
//! 1. Server-side: route handler files → `response_shapes::extract`
//!    → `RouteShape { response_keys, error_keys }` attached to the
//!    Route node.
//! 2. Client-side: consumer files containing `fetch()` /
//!    `axios.get()` / etc. → `consumer_keys::extract` → keys encoded
//!    into the `Fetches` edge's `reason` field.
//! 3. `ecp shape_check` parses the reason via [`parse_reason`] and
//!    compares against the Route's `response_keys ∪ error_keys`.
//!
//! Reason wire format (matches upstream verbatim for cross-port
//! compat): `fetch-url-match[|keys:a,b][|fetches:N]`. The base tag
//! is always present; `keys:` is omitted when no keys were
//! extracted; `fetches:` is omitted when the consumer file matched
//! only a single route (the default fetch count).

pub mod consumer_keys;
pub mod fetch_urls;
pub mod response_shapes;

/// Normalize an HTTP route path for `(method, path)` index lookup.
///
/// Applied identically to both the route-server side and the HTTP-client
/// call side so that `app.get('/api/users/:id', h)` and
/// `fetch('/api/users/:id')` both normalize to `"api/users/:*"`.
///
/// Rules (applied in order):
/// 1. If the path starts with `http://` or `https://`, the scheme+host are
///    NOT stripped — external-host URLs intentionally fail to match in-graph
///    routes (cross-repo topology is handled by `ecp contracts`, not here).
///    Only relative paths (starting with `/`) are matched.
/// 2. Strip query string (`/api/users?page=2` → `/api/users`).
/// 3. Strip leading and trailing `/`.
/// 4. Replace every path segment that is a parameter placeholder
///    (`:param`, `{param}`, `<param>`, or `${expr}`) with `:*`.
///
/// Returns `(normalized_path, is_templated)`.  `is_templated` is `true`
/// when at least one placeholder was replaced — the builder uses this to
/// pick confidence 0.6 (templated) vs 0.8 (exact).
pub fn normalize_route_path(path: &str) -> (String, bool) {
    // Strip query string.
    let path = path.split('?').next().unwrap_or(path);

    // Strip leading `/` and trailing `/`.
    let path = path.trim_start_matches('/').trim_end_matches('/');

    // Normalize per-segment: `:param` / `{param}` / `<param>` / `${expr}` → `:*`.
    let mut is_templated = false;
    let normalized = path
        .split('/')
        .map(|seg| {
            if seg.starts_with(':')
                || (seg.starts_with('{') && seg.ends_with('}'))
                || (seg.starts_with('<') && seg.ends_with('>'))
                || seg.contains("${")
            {
                is_templated = true;
                ":*"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/");

    (normalized, is_templated)
}

/// Base reason tag every Fetches edge carries. Downstream parsers
/// reject reasons that don't start with this token.
pub const REASON_TAG: &str = "fetch-url-match";

/// Build the `Edge.reason` string for a `RelType::Fetches` edge.
/// `keys` are the consumer-side accessed keys (already deduped by the
/// extractor). `fetch_count` is how many distinct routes the consumer
/// file fetches; values ≤ 1 are omitted from the wire format because
/// they are the default.
pub fn format_reason(keys: &[String], fetch_count: u32) -> String {
    let mut s = String::from(REASON_TAG);
    if !keys.is_empty() {
        s.push_str("|keys:");
        s.push_str(&keys.join(","));
    }
    if fetch_count > 1 {
        s.push_str("|fetches:");
        s.push_str(&fetch_count.to_string());
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedReason {
    pub keys: Vec<String>,
    pub fetch_count: u32,
}

/// Parse an `Edge.reason` produced by [`format_reason`]. Returns
/// `None` if the reason is not a Fetches reason (i.e., doesn't start
/// with [`REASON_TAG`]); shape_check treats `None` as "skip this edge".
/// Unknown trailing segments are ignored — forward-compat with future
/// reason annotations.
pub fn parse_reason(reason: &str) -> Option<ParsedReason> {
    let mut parts = reason.split('|');
    if parts.next()? != REASON_TAG {
        return None;
    }
    let mut keys = Vec::new();
    let mut fetch_count: u32 = 1;
    for part in parts {
        if let Some(rest) = part.strip_prefix("keys:") {
            keys = rest
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        } else if let Some(rest) = part.strip_prefix("fetches:") {
            if let Ok(n) = rest.parse() {
                fetch_count = n;
            }
        }
    }
    Some(ParsedReason { keys, fetch_count })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize_route_path ─────────────────────────────────────────────

    #[test]
    fn normalize_strips_leading_slash() {
        let (p, tmpl) = normalize_route_path("/api/users");
        assert_eq!(p, "api/users");
        assert!(!tmpl);
    }

    #[test]
    fn normalize_express_param() {
        let (p, tmpl) = normalize_route_path("/api/users/:id");
        assert_eq!(p, "api/users/:*");
        assert!(tmpl);
    }

    #[test]
    fn normalize_brace_param() {
        let (p, tmpl) = normalize_route_path("/api/users/{id}");
        assert_eq!(p, "api/users/:*");
        assert!(tmpl);
    }

    #[test]
    fn normalize_angle_param() {
        let (p, tmpl) = normalize_route_path("/api/users/<id>");
        assert_eq!(p, "api/users/:*");
        assert!(tmpl);
    }

    #[test]
    fn normalize_dollar_brace_interp() {
        // Client-side template literal `/api/users/${id}`
        let (p, tmpl) = normalize_route_path("/api/users/${id}");
        assert_eq!(p, "api/users/:*");
        assert!(tmpl);
    }

    #[test]
    fn normalize_strips_query_string() {
        let (p, tmpl) = normalize_route_path("/api/users?page=2");
        assert_eq!(p, "api/users");
        assert!(!tmpl);
    }

    #[test]
    fn normalize_external_host_not_stripped() {
        // External-host URLs are NOT normalized to path-only — they intentionally
        // produce a different normalized form than in-graph routes so they never
        // match (cross-repo topology handled by ecp contracts).
        let (p, _tmpl) = normalize_route_path("https://api.example.com/api/users");
        assert_ne!(p, "api/users", "external host must not be stripped");
        // The normalized form retains the host component (after stripping leading `/`).
        assert!(p.contains("api.example.com"), "host retained: {p}");
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        let (p, tmpl) = normalize_route_path("/api/users/");
        assert_eq!(p, "api/users");
        assert!(!tmpl);
    }

    #[test]
    fn normalize_exact_path_not_templated() {
        let (p, tmpl) = normalize_route_path("/api/users/42");
        assert_eq!(p, "api/users/42");
        assert!(!tmpl);
    }

    // ── format_reason / parse_reason ────────────────────────────────────

    #[test]
    fn format_bare_no_keys_no_count() {
        assert_eq!(format_reason(&[], 1), "fetch-url-match");
    }

    #[test]
    fn format_with_keys() {
        assert_eq!(
            format_reason(&["id".into(), "name".into()], 1),
            "fetch-url-match|keys:id,name",
        );
    }

    #[test]
    fn format_with_multi_fetch() {
        assert_eq!(
            format_reason(&["id".into()], 3),
            "fetch-url-match|keys:id|fetches:3",
        );
    }

    #[test]
    fn parse_roundtrip() {
        let original = format_reason(&["foo".into(), "bar".into()], 2);
        let parsed = parse_reason(&original).expect("valid reason");
        assert_eq!(parsed.keys, vec!["foo", "bar"]);
        assert_eq!(parsed.fetch_count, 2);
    }

    #[test]
    fn parse_unknown_tag_returns_none() {
        assert!(parse_reason("some-other-edge").is_none());
        assert!(parse_reason("").is_none());
    }

    #[test]
    fn parse_unknown_segment_ignored() {
        let parsed = parse_reason("fetch-url-match|keys:x|future:tag").unwrap();
        assert_eq!(parsed.keys, vec!["x"]);
        assert_eq!(parsed.fetch_count, 1);
    }

    #[test]
    fn parse_empty_keys_yields_empty_vec() {
        let parsed = parse_reason("fetch-url-match|keys:").unwrap();
        assert!(parsed.keys.is_empty());
    }
}
