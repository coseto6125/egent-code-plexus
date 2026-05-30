use ecp_analyzer::sql_literal::{is_sql_shaped, parse_tables};
use ecp_core::analyzer::types::SqlVerb;

#[test]
fn is_sql_shaped_accepts_select_rejects_prose() {
    assert!(is_sql_shaped("SELECT id FROM channels WHERE org_id = $1"));
    assert!(is_sql_shaped("INSERT INTO channels (a) VALUES ($1)"));
    assert!(!is_sql_shaped("syncing channels for org"));
    assert!(!is_sql_shaped("user logged in successfully"));
    assert!(!is_sql_shaped(""));
}

#[test]
fn parse_tables_select_is_read() {
    let r = parse_tables("SELECT id, slug FROM channels WHERE org_id = $1");
    assert!(!r.unresolved);
    assert_eq!(r.tables, vec![("channels".to_string(), SqlVerb::Read)]);
}

#[test]
fn parse_tables_insert_update_delete_are_write() {
    for sql in [
        "INSERT INTO channels (slug) VALUES ($1)",
        "UPDATE channels SET slug = $1 WHERE id = $2",
        "DELETE FROM channels WHERE id = $1",
    ] {
        let r = parse_tables(sql);
        assert!(!r.unresolved, "sql={sql}");
        assert_eq!(
            r.tables,
            vec![("channels".to_string(), SqlVerb::Write)],
            "sql={sql}"
        );
    }
}

#[test]
fn parse_tables_join_collects_both_tables_not_column_qualifiers() {
    let r = parse_tables("SELECT a FROM channels c JOIN bots b ON c.x = b.y");
    assert!(!r.unresolved);
    let names: Vec<&str> = r.tables.iter().map(|(t, _)| t.as_str()).collect();
    assert!(names.contains(&"channels"));
    assert!(names.contains(&"bots"));
    assert!(!names.contains(&"c") && !names.contains(&"b"));
}

#[test]
fn parse_tables_unparseable_is_unresolved() {
    let r = parse_tables("this is not sql at all FROM");
    assert!(r.unresolved);
    assert!(r.tables.is_empty());
}

#[test]
fn parse_tables_interpolated_table_is_unresolved() {
    // A placeholder/interpolation in the table position is not a real identifier.
    let r = parse_tables("SELECT * FROM {tbl} WHERE id = $1");
    assert!(r.unresolved);
}

#[test]
fn parse_tables_cte_alias_is_not_a_table() {
    let r = parse_tables("WITH t AS (SELECT id FROM channels) SELECT * FROM t");
    let names: Vec<&str> = r.tables.iter().map(|(t, _)| t.as_str()).collect();
    assert!(
        names.contains(&"channels"),
        "real table channels must be present"
    );
    assert!(!names.contains(&"t"), "CTE alias t must NOT be a table");
}

#[test]
fn parse_tables_insert_select_separates_read_and_write_verbs() {
    let r = parse_tables("INSERT INTO channels (id) SELECT id FROM bots");
    // channels is the insert target → Write; bots is the select source → Read.
    assert!(
        r.tables.contains(&("channels".to_string(), SqlVerb::Write)),
        "channels must be Write, got {:?}",
        r.tables
    );
    assert!(
        r.tables.contains(&("bots".to_string(), SqlVerb::Read)),
        "bots must be Read, got {:?}",
        r.tables
    );
}
