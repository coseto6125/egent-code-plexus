use ecp_core::registry::{CommitDirName, DirNameParseError as ParseError, Generation, SourceType};

#[test]
fn parse_branch_simple() {
    let n = CommitDirName::parse("branch_main__abc123def4567890abc123def4567890abc123de").unwrap();
    assert_eq!(n.source_type, SourceType::Branch);
    assert_eq!(n.source_id.as_deref(), Some("main"));
    assert_eq!(n.sha_hex(), "abc123def4567890abc123def4567890abc123de");
}

#[test]
fn parse_commit_no_id() {
    let n = CommitDirName::parse("commit__456789abc123def456789abc123def456789abc1").unwrap();
    assert_eq!(n.source_type, SourceType::Commit);
    assert!(n.source_id.is_none());
}

#[test]
fn parse_source_id_with_underscore() {
    let n =
        CommitDirName::parse("branch_feat_x_v2__abc123def4567890abc123def4567890abc123de").unwrap();
    assert_eq!(n.source_id.as_deref(), Some("feat_x_v2"));
}

#[test]
fn parse_source_id_with_double_underscore() {
    // rsplit_once("__") cuts from the right so the sha segment wins
    let n = CommitDirName::parse("branch_weird__name__abc123def4567890abc123def4567890abc123de")
        .unwrap();
    assert_eq!(n.source_id.as_deref(), Some("weird__name"));
}

#[test]
fn parse_generation_suffix_single_integer_back_compat() {
    let n =
        CommitDirName::parse("branch_main__abc123def4567890abc123def4567890abc123de.gen.20260520")
            .unwrap();
    assert_eq!(n.source_type, SourceType::Branch);
    assert_eq!(n.source_id.as_deref(), Some("main"));
    assert_eq!(n.sha_hex(), "abc123def4567890abc123def4567890abc123de");
    // Older single-int suffix lifts into `(t, 0, 0)` so it still compares
    // correctly under lex order vs the current 3-tuple format.
    assert_eq!(
        n.generation,
        Some(Generation {
            timestamp_ms: 20260520,
            pid: 0,
            counter: 0,
        })
    );
}

#[test]
fn parse_generation_three_tuple_current_format() {
    // Current `publish_dir_for` writes `.gen.{timestamp_ms}.{pid}.{counter}`.
    let n = CommitDirName::parse(
        "branch_main__abc123def4567890abc123def4567890abc123de.gen.1730000000123.4567.42",
    )
    .unwrap();
    assert_eq!(
        n.generation,
        Some(Generation {
            timestamp_ms: 1_730_000_000_123,
            pid: 4567,
            counter: 42,
        })
    );
}

#[test]
fn parse_no_generation_returns_none() {
    let n = CommitDirName::parse("branch_main__abc123def4567890abc123def4567890abc123de").unwrap();
    // Base dirs (no `.gen.` suffix) sort BELOW any generation dir for the
    // same SHA via `None < Some(_)` total order.
    assert_eq!(n.generation, None);
}

#[test]
fn parse_invalid_generation_suffix_is_none_not_error() {
    // Garbage after `.gen.` is tolerated — the SHA still parses, generation
    // falls back to None so the dir loses every same-SHA tie. Worst case
    // it gets shadowed by a clean base dir, which the next reindex fixes.
    let n = CommitDirName::parse(
        "branch_main__abc123def4567890abc123def4567890abc123de.gen.not-a-number",
    )
    .unwrap();
    assert_eq!(n.sha_hex(), "abc123def4567890abc123def4567890abc123de");
    assert_eq!(n.generation, None);
}

#[test]
fn generation_lex_order_total_across_three_tuple() {
    let earlier = Generation {
        timestamp_ms: 100,
        pid: 1,
        counter: 1,
    };
    let later = Generation {
        timestamp_ms: 100,
        pid: 1,
        counter: 2,
    };
    let way_later = Generation {
        timestamp_ms: 200,
        pid: 1,
        counter: 0,
    };
    assert!(earlier < later);
    assert!(later < way_later);
    // None < Some — base dirs always lose to generation dirs.
    let base: Option<Generation> = None;
    let gen = Some(earlier);
    assert!(base < gen);
}

#[test]
fn round_trip_with_three_tuple_generation() {
    let original =
        "branch_main__abc123def4567890abc123def4567890abc123de.gen.1730000000123.4567.42";
    let parsed = CommitDirName::parse(original).unwrap();
    assert_eq!(parsed.format(), original);
}

#[test]
fn reject_unknown_source_type() {
    assert!(matches!(
        CommitDirName::parse("fake_x__abc123def4567890abc123def4567890abc123de"),
        Err(ParseError::UnknownSourceType(_))
    ));
}

#[test]
fn reject_non_hex_sha() {
    assert!(matches!(
        CommitDirName::parse("branch_main__notahexstring1234567890abcdef12345xyz"),
        Err(ParseError::InvalidSha)
    ));
}

#[test]
fn reject_short_sha() {
    assert!(matches!(
        CommitDirName::parse("branch_main__abc123"),
        Err(ParseError::InvalidSha)
    ));
}

#[test]
fn round_trip_format_parse() {
    let original = "tag_v1.2.3__789abc123def456789abc123def456789abc1234";
    let parsed = CommitDirName::parse(original).unwrap();
    assert_eq!(parsed.format(), original);
}

#[test]
fn reject_no_separator() {
    assert!(matches!(
        CommitDirName::parse("branch_main_abc123def4567890abc123def4567890abc1"),
        Err(ParseError::NoSha)
    ));
}

#[test]
fn parse_tag_simple() {
    let n = CommitDirName::parse("tag_v1.2.3__789abc123def456789abc123def456789abc1234").unwrap();
    assert_eq!(n.source_type, SourceType::Tag);
    assert_eq!(n.source_id.as_deref(), Some("v1.2.3"));
}

#[test]
fn parse_pr_simple() {
    let n = CommitDirName::parse("pr_123__abc123def4567890abc123def4567890abc123de").unwrap();
    assert_eq!(n.source_type, SourceType::Pr);
    assert_eq!(n.source_id.as_deref(), Some("123"));
}
