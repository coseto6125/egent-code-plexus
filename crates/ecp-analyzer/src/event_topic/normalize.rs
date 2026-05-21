use regex::Regex;
use std::sync::OnceLock;

/// Canonicalizes event topic identifiers per docs/specs/2026-05-21-event-topic-normalization.md.
///
/// Rules applied in order:
/// 1. Strip environment prefixes: `prod.`, `dev.`, `staging.`, `<env>.`
/// 2. Strip version suffix: `.v[0-9]+`
/// 3. Lowercase
/// 4. Replace `.`, `_`, `-`, `:`, `/` with `/`
/// 5. Trim leading/trailing `/`
/// 6. CamelCase→snake_case per segment
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

/// Convert a single segment from CamelCase to slash-separated lowercase words.
/// Splits on lowercase→uppercase transitions.
/// E.g., `OrderCreated` → `Order/Created`, `userSignedUp` → `user/Signed/Up`.
fn split_camel_case(segment: &str) -> String {
    if segment.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    let chars: Vec<char> = segment.chars().collect();

    for (i, &ch) in chars.iter().enumerate() {
        if i > 0 && ch.is_uppercase() && chars[i - 1].is_lowercase() {
            // Transition from lowercase to uppercase: split here
            result.push('/');
        }
        result.push(ch);
    }

    result
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
}
