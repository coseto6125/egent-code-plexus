use ecp_core::graph::{NodeKind, RelType};
use std::str::FromStr;

#[test]
fn test_from_str_roundtrip_all_new_variants() {
    let node_cases = [
        ("schemafield", NodeKind::SchemaField),
        ("schema_field", NodeKind::SchemaField),
        ("eventtopic", NodeKind::EventTopic),
        ("event_topic", NodeKind::EventTopic),
        ("transactionscope", NodeKind::TransactionScope),
        ("transaction_scope", NodeKind::TransactionScope),
        ("enumvariant", NodeKind::EnumVariant),
        ("enum_variant", NodeKind::EnumVariant),
    ];
    for (s, expected) in node_cases {
        let got = NodeKind::from_str(s).unwrap_or_else(|_| panic!("from_str({s:?}) failed"));
        assert_eq!(got, expected, "from_str({s:?})");
        assert_eq!(
            got.as_str(),
            expected.as_str(),
            "as_str roundtrip for {s:?}"
        );
    }

    let rel_cases = [
        ("MIRRORSFIELD", RelType::MirrorsField),
        ("MIRRORS_FIELD", RelType::MirrorsField),
        ("PUBLISHES", RelType::Publishes),
        ("SUBSCRIBES", RelType::Subscribes),
        ("EVENTTOPICMIRROR", RelType::EventTopicMirror),
        ("EVENT_TOPIC_MIRROR", RelType::EventTopicMirror),
        ("OPENSTXSCOPE", RelType::OpensTxScope),
        ("OPENS_TX_SCOPE", RelType::OpensTxScope),
    ];
    for (s, expected) in rel_cases {
        let got = RelType::from_str(s).unwrap_or_else(|_| panic!("from_str({s:?}) failed"));
        assert_eq!(got, expected, "from_str({s:?})");
    }
}

#[test]
fn test_node_kind_discriminants_locked() {
    assert_eq!(NodeKind::SchemaField as u8, 24, "SchemaField discriminant");
    assert_eq!(NodeKind::EventTopic as u8, 25, "EventTopic discriminant");
    assert_eq!(
        NodeKind::TransactionScope as u8,
        26,
        "TransactionScope discriminant"
    );
    assert_eq!(NodeKind::EnumVariant as u8, 27, "EnumVariant discriminant");
}

#[test]
fn test_rel_type_discriminants_locked() {
    assert_eq!(RelType::MirrorsField as u8, 12, "MirrorsField discriminant");
    assert_eq!(RelType::Publishes as u8, 13, "Publishes discriminant");
    assert_eq!(RelType::Subscribes as u8, 14, "Subscribes discriminant");
    assert_eq!(
        RelType::EventTopicMirror as u8,
        15,
        "EventTopicMirror discriminant"
    );
    assert_eq!(RelType::OpensTxScope as u8, 16, "OpensTxScope discriminant");
}

#[test]
fn test_is_heuristic_classification() {
    assert!(RelType::MirrorsField.is_heuristic());
    assert!(RelType::EventTopicMirror.is_heuristic());

    // All non-heuristic variants must return false.
    let non_heuristic = [
        RelType::Defines,
        RelType::Imports,
        RelType::Calls,
        RelType::Extends,
        RelType::Implements,
        RelType::HasMethod,
        RelType::HasProperty,
        RelType::Accesses,
        RelType::HandlesRoute,
        RelType::StepInProcess,
        RelType::References,
        RelType::Fetches,
        RelType::Publishes,
        RelType::Subscribes,
        RelType::OpensTxScope,
    ];
    for rel in non_heuristic {
        assert!(!rel.is_heuristic(), "{rel:?} should not be heuristic");
    }
}
