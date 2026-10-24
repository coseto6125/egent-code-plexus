//! The stored uid-collision hint is machine-parsed (U+001F separated), so it
//! stays as is. What an agent reads must come from the structured fields:
//! no raw separator, and the colliding declaration's line number.

use ecp_core::graph::{format_hint, render_hint, HintFields, HINT_FIELD_SEP};

fn fields<'a>(path: &'a str, owner: &'a str, name: &'a str) -> HintFields<'a> {
    HintFields {
        kind: "Function",
        path,
        owner,
        name,
    }
}

#[test]
fn test_render_hint_has_no_separator_and_carries_line_of_second() {
    let stored = format_hint(
        "uid-collision",
        fields("crates/a/fs_safe.rs", "", "with_windows_retry"),
        fields("crates/a/fs_safe.rs", "", "with_windows_retry"),
    );
    let shown = render_hint(&stored, 43);
    assert!(!shown.contains(HINT_FIELD_SEP), "{shown:?}");
    assert!(shown.starts_with("uid-collision: "), "{shown}");
    assert!(
        shown.contains("second=Function crates/a/fs_safe.rs:43 with_windows_retry"),
        "{shown}"
    );
    assert!(
        shown.contains("first=Function crates/a/fs_safe.rs with_windows_retry"),
        "{shown}"
    );
}

#[test]
fn test_render_hint_joins_owner_and_name() {
    let stored = format_hint(
        "uid-collision",
        fields("a.rs", "Foo", "bar"),
        fields("b.rs", "Foo", "bar"),
    );
    let shown = render_hint(&stored, 7);
    assert!(shown.contains("first=Function a.rs Foo.bar"), "{shown}");
    assert!(shown.contains("second=Function b.rs:7 Foo.bar"), "{shown}");
}

#[test]
fn test_render_hint_not_a_collision_hint_is_returned_unchanged() {
    assert_eq!(
        render_hint("eval at dynamic site", 3),
        "eval at dynamic site"
    );
    assert_eq!(render_hint("", 3), "");
    assert_eq!(render_hint("   ", 3), "   ");
}

#[test]
fn test_render_hint_truncated_fields_fall_back_without_separator() {
    let broken = format!("uid-collision: first=Function{HINT_FIELD_SEP}a.rs second=Function");
    let shown = render_hint(&broken, 1);
    assert!(!shown.contains(HINT_FIELD_SEP), "{shown:?}");
}
