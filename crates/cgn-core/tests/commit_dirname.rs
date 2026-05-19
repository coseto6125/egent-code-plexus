use cgn_core::registry::{CommitDirName, DirNameParseError as ParseError, SourceType};

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
fn parse_generation_suffix() {
    let n =
        CommitDirName::parse("branch_main__abc123def4567890abc123def4567890abc123de.gen.20260520")
            .unwrap();
    assert_eq!(n.source_type, SourceType::Branch);
    assert_eq!(n.source_id.as_deref(), Some("main"));
    assert_eq!(n.sha_hex(), "abc123def4567890abc123def4567890abc123de");
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
