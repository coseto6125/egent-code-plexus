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

// Golden (discriminant -> variant name) tables for BOTH enums. rkyv archives
// store raw discriminants, so any reorder/insert/rename silently re-labels
// every node and edge in every existing `graph.bin` — queries return wrong
// answers without crashing. A failure here means the enum changed shape:
// the ONLY allowed fix is appending the new variant at the END of the
// matching golden list. Never reorder these lists to make the test pass.

#[test]
fn test_node_kind_discriminants_locked() {
    const GOLDEN: [&str; 29] = [
        "File",
        "Function",
        "Class",
        "Method",
        "Interface",
        "Constructor",
        "Property",
        "Variable",
        "Const",
        "Import",
        "Route",
        "Process",
        "Document",
        "Section",
        "EntryPoint",
        "Struct",
        "Enum",
        "Typedef",
        "Namespace",
        "Module",
        "Macro",
        "Annotation",
        "Trait",
        "Impl",
        "SchemaField",
        "EventTopic",
        "TransactionScope",
        "EnumVariant",
        "PathLiteral",
    ];
    assert_eq!(NodeKind::ALL.len(), GOLDEN.len(), "NodeKind variant count");
    for (i, (kind, expected)) in NodeKind::ALL.iter().zip(GOLDEN).enumerate() {
        assert_eq!(*kind as u8 as usize, i, "{expected} discriminant moved");
        assert_eq!(kind.as_str(), expected, "name at discriminant {i}");
    }
}

#[test]
fn test_rel_type_discriminants_locked() {
    const GOLDEN: [&str; 23] = [
        "Defines",
        "Imports",
        "Calls",
        "Extends",
        "Implements",
        "HasMethod",
        "HasProperty",
        "Accesses",
        "HandlesRoute",
        "StepInProcess",
        "References",
        "Fetches",
        "MirrorsField",
        "Publishes",
        "Subscribes",
        "EventTopicMirror",
        "OpensTxScope",
        "Overrides",
        "Decorates",
        "UsesPathLiteral",
        "ReadsField",
        "CompensatedBy",
        "QueriesTable",
    ];
    assert_eq!(RelType::ALL.len(), GOLDEN.len(), "RelType variant count");
    for (i, (rel, expected)) in RelType::ALL.iter().zip(GOLDEN).enumerate() {
        assert_eq!(*rel as u8 as usize, i, "{expected} discriminant moved");
        assert_eq!(rel.as_str(), expected, "name at discriminant {i}");
    }
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
