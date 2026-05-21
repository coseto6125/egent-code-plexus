use regex::Regex;
use std::sync::OnceLock;

/// Canonicalizes event topic identifiers per docs/specs/2026-05-21-event-topic-normalization.md.
///
/// Rules applied in this order (note: 3 is applied last, after 6, to preserve
/// case information for CamelCase detection):
/// 1. Strip environment prefixes: `prod.`, `dev.`, `staging.`, `<env>.`
/// 2. Strip version suffix: `.v[0-9]+`
/// 4. Replace `.`, `_`, `-`, `:`, `/` with `/`
/// 5. Trim leading/trailing `/`
/// 6. CamelCase→slash-separated per segment (two rules: lower→upper, and
///    upper+upper→lower to handle acronym prefixes like `HTTP`, `XML`)
/// 3. Lowercase (applied after 6 so case info is available for splitting)
pub fn canonicalize(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }

    let mut result = s.to_string();

    // Rule 1: Strip environment prefixes (prod., dev., staging.)
    for env_prefix in &["prod.", "dev.", "staging."] {
        if result.starts_with(env_prefix) {
            result = result[env_prefix.len()..].to_string();
            break;
        }
    }

    // Rule 2: Strip version suffix (.v[0-9]+)
    let version_re = get_version_regex();
    result = version_re.replace(&result, "").into_owned();

    // Rule 4: Replace separators with / (before lowercase to preserve camelCase)
    result = result.replace(&['_', '-', ':', '.'][..], "/");

    // Handle consecutive slashes (from adjacent separators)
    result = result.replace("//", "/");

    // Rule 5: Trim leading/trailing /
    result = result.trim_matches('/').to_string();

    // Rule 6: CamelCase→slash-separated per segment (before lowercase to detect case)
    let segments: Vec<&str> = result.split('/').collect();
    let camel_segments: Vec<String> = segments.into_iter().map(split_camel_case).collect();
    result = camel_segments.join("/");

    // Handle consecutive slashes that may have been created
    result = result.replace("//", "/");

    // Rule 3: Lowercase (after camelCase conversion to preserve case info)
    result.to_lowercase()
}

fn get_version_regex() -> &'static Regex {
    static VERSION_REGEX: OnceLock<Regex> = OnceLock::new();
    VERSION_REGEX.get_or_init(|| Regex::new(r"\.v\d+$").expect("valid regex"))
}

/// Convert a single segment from CamelCase to slash-separated words.
///
/// Two split rules (applied left-to-right):
/// - Rule A: lowercase → uppercase transition (`userCreated` → `user/Created`).
/// - Rule B: uppercase + uppercase → lowercase transition, i.e. the start of a
///   Capitalized word that follows an acronym run (`HTTPServer` → `HTTP/Server`,
///   `XMLParser` → `XML/Parser`, `HTTPStatusOk` → `HTTP/Status/Ok`).
///
/// E.g., `OrderCreated` → `Order/Created`, `userSignedUp` → `user/Signed/Up`.
fn split_camel_case(segment: &str) -> String {
    if segment.is_empty() {
        return String::new();
    }

    let chars: Vec<char> = segment.chars().collect();
    let mut splits = vec![0usize];

    for i in 1..chars.len() {
        let prev = chars[i - 1];
        let curr = chars[i];
        let next = chars.get(i + 1).copied();

        // Rule A: lowercase → uppercase
        let rule_a = prev.is_ascii_lowercase() && curr.is_ascii_uppercase();
        // Rule B: uppercase → uppercase → lowercase (acronym end / word start)
        let rule_b = prev.is_ascii_uppercase()
            && curr.is_ascii_uppercase()
            && next.is_some_and(|c| c.is_ascii_lowercase());

        if rule_a || rule_b {
            splits.push(i);
        }
    }
    splits.push(chars.len());

    splits
        .windows(2)
        .map(|w| chars[w[0]..w[1]].iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_string() {
        assert_eq!(canonicalize(""), "");
    }

    #[test]
    fn test_prod_prefix_stripped() {
        assert_eq!(canonicalize("prod.order.created"), "order/created");
    }

    #[test]
    fn test_dev_prefix_stripped() {
        assert_eq!(canonicalize("dev.order.created"), "order/created");
    }

    #[test]
    fn test_staging_prefix_stripped() {
        assert_eq!(canonicalize("staging.order.created"), "order/created");
    }

    #[test]
    fn test_version_suffix_stripped() {
        assert_eq!(canonicalize("order.created.v1"), "order/created");
        assert_eq!(canonicalize("order.created.v123"), "order/created");
    }

    #[test]
    fn test_lowercase() {
        assert_eq!(canonicalize("ORDER_CREATED"), "order/created");
    }

    #[test]
    fn test_underscore_to_slash() {
        assert_eq!(canonicalize("order_created"), "order/created");
    }

    #[test]
    fn test_hyphen_to_slash() {
        assert_eq!(canonicalize("order-created"), "order/created");
    }

    #[test]
    fn test_dot_to_slash() {
        assert_eq!(canonicalize("order.created"), "order/created");
    }

    #[test]
    fn test_colon_to_slash() {
        assert_eq!(canonicalize("order:created"), "order/created");
    }

    #[test]
    fn test_slash_unchanged() {
        assert_eq!(canonicalize("order/created"), "order/created");
    }

    #[test]
    fn test_hyphen_and_slash_collapse_to_same_canonical() {
        // Negative documentation: both should normalize to the same value
        assert_eq!(canonicalize("order-created"), canonicalize("order/created"));
        assert_eq!(canonicalize("order-created"), "order/created");
    }

    #[test]
    fn test_region_prefixes_stay_distinct() {
        // Negative documentation: region prefixes are preserved (not env prefixes)
        assert_ne!(
            canonicalize("eu-west-1.order.created"),
            canonicalize("eu-west-2.order.created")
        );
        assert_eq!(
            canonicalize("eu-west-1.order.created"),
            "eu/west/1/order/created"
        );
        assert_eq!(
            canonicalize("eu-west-2.order.created"),
            "eu/west/2/order/created"
        );
    }

    #[test]
    fn test_tenant_ids_stay_distinct() {
        // Negative documentation: tenant IDs are preserved
        assert_ne!(
            canonicalize("tenant-123.order.created"),
            canonicalize("tenant-456.order.created")
        );
        assert_eq!(
            canonicalize("tenant-123.order.created"),
            "tenant/123/order/created"
        );
        assert_eq!(
            canonicalize("tenant-456.order.created"),
            "tenant/456/order/created"
        );
    }

    #[test]
    fn test_camel_case_simple() {
        assert_eq!(canonicalize("OrderCreated"), "order/created");
    }

    #[test]
    fn test_camel_case_multi_word() {
        assert_eq!(canonicalize("userSignedUp"), "user/signed/up");
    }

    #[test]
    fn test_combined_env_version_camel() {
        assert_eq!(canonicalize("prod.UserSignedUp.v2"), "user/signed/up");
    }

    #[test]
    fn test_mixed_separators() {
        assert_eq!(
            canonicalize("prod.order_Status-Update.v1"),
            "order/status/update"
        );
    }

    #[test]
    fn test_all_uppercase() {
        assert_eq!(canonicalize("ORDER_STATUS_UPDATE"), "order/status/update");
    }

    #[test]
    fn test_single_word() {
        assert_eq!(canonicalize("order"), "order");
    }

    #[test]
    fn test_leading_trailing_slashes() {
        assert_eq!(canonicalize("/order/created/"), "order/created");
    }

    #[test]
    fn test_consecutive_separators() {
        assert_eq!(canonicalize("order..created"), "order/created");
        assert_eq!(canonicalize("order__created"), "order/created");
    }

    #[test]
    fn test_complex_real_world() {
        assert_eq!(
            canonicalize("prod.user_PasswordChanged.v3"),
            "user/password/changed"
        );
    }

    // ── split_camel_case unit tests ────────────────────────────────────────

    #[test]
    fn split_camel_case_simple_transition() {
        // Rule A only — existing behaviour must not regress
        assert_eq!(split_camel_case("OrderCreated"), "Order/Created");
        assert_eq!(split_camel_case("userSignedUp"), "user/Signed/Up");
    }

    #[test]
    fn split_camel_case_handles_consecutive_capitals() {
        // Rule B: acronym prefix followed by capitalized word
        assert_eq!(split_camel_case("HTTPServer"), "HTTP/Server");
        assert_eq!(split_camel_case("XMLParser"), "XML/Parser");
        assert_eq!(split_camel_case("HTTPStatusOk"), "HTTP/Status/Ok");
    }

    // ── canonicalize integration tests for acronym-prefixed topics ─────────

    #[test]
    fn test_camel_case_acronym_prefix_http_server() {
        assert_eq!(canonicalize("HTTPServer"), "http/server");
    }

    #[test]
    fn test_camel_case_acronym_prefix_xml_parser() {
        assert_eq!(canonicalize("XMLParser"), "xml/parser");
    }

    #[test]
    fn test_camel_case_acronym_prefix_http_status_ok() {
        assert_eq!(canonicalize("HTTPStatusOk"), "http/status/ok");
    }
}
