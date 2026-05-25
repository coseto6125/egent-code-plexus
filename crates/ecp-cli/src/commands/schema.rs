use clap::{Args, Subcommand};
use ecp_core::EcpError;
use serde::Serialize;

#[derive(Args, Clone)]
pub struct SchemaArgs {
    #[command(subcommand)]
    pub command: SchemaCommands,
}

#[derive(Subcommand, Clone)]
pub enum SchemaCommands {
    /// Per-language BlindSpot emitter inventory.
    ///
    /// Distinguishes "no blind spot in this diff" from "ecp doesn't detect
    /// this dispatch pattern in this language yet" — exactly the
    /// LLM-context signal Constraint 5 of the cross-lang spec requires.
    Blindspots(FormatArgs),
    /// Inventory of every `RelType` edge label, paired with the LLM-utility
    /// rationale (graph-completeness / node-coverage / edge-semantics) so an
    /// agent can pick the right Cypher rel without guessing.
    Reltypes(FormatArgs),
    /// Inventory of every `NodeKind` variant. Highlights the same-name
    /// distinctions that CLAUDE.md flags as load-bearing (e.g. `Struct` vs
    /// `Class`, `Trait` vs `Interface`, `Enum` distinct from OO conventions).
    NodeKinds(FormatArgs),
    /// Current rkyv `graph.bin` format version + last-bump rationale. Used
    /// after a schema-affecting commit lands so consumers know whether to
    /// trigger a re-index.
    GraphVersion(FormatArgs),
}

#[derive(Args, Clone)]
pub struct FormatArgs {
    /// Output format. Default `json`; `text` is a human-readable table.
    #[arg(long, default_value = "json")]
    pub format: String,
}

/// Detection status for a per-lang capability. Append-only enum (string
/// JSON discriminator — no rkyv discriminant to worry about).
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Status {
    Implemented,
    None,
}

#[derive(Serialize)]
struct LangEntry {
    name: &'static str,
    blindspot_emitter: Status,
    indirect_dispatch: Status,
    blind_kinds: &'static [&'static str],
}

#[derive(Serialize)]
struct BlindspotsReport {
    languages: &'static [LangEntry],
}

/// Per-lang inventory. Hardcoded because the per-parser `BLIND_SPEC`
/// tables are `pub(crate)` and reflecting them at runtime is overkill for
/// a stable spec. Kept beside `parse_file` impl in each parser; whoever
/// adds a new lang updates both places.
///
/// Order: same as in `~/.ecp/index/{repo}/lang_counts.json` (alphabetical
/// for stable diff output).
const LANGUAGES: &[LangEntry] = &[
    LangEntry {
        name: "c",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::Implemented,
        blind_kinds: &["c-dlsym"],
    },
    LangEntry {
        name: "cpp",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::Implemented,
        blind_kinds: &["cpp-dlsym"],
    },
    LangEntry {
        name: "c_sharp",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::None,
        blind_kinds: &["cs-activator-create-instance", "cs-method-invoke"],
    },
    LangEntry {
        name: "dart",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::None,
        blind_kinds: &["dart-function-apply", "dart-mirrors-import"],
    },
    LangEntry {
        name: "go",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::None,
        blind_kinds: &["go-reflect-method-by-name", "go-plugin-open"],
    },
    LangEntry {
        name: "java",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::None,
        blind_kinds: &["java-class-forname", "java-method-invoke"],
    },
    LangEntry {
        name: "javascript",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::Implemented,
        blind_kinds: &[
            "js-eval",
            "js-function-ctor",
            "js-dynamic-import",
            "js-dynamic-require",
        ],
    },
    LangEntry {
        name: "kotlin",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::None,
        blind_kinds: &["kt-class-forname", "kt-method-invoke"],
    },
    LangEntry {
        name: "php",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::None,
        blind_kinds: &["php-eval", "php-call-user-func", "php-variable-call"],
    },
    LangEntry {
        name: "python",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::Implemented,
        blind_kinds: &[
            "python-eval",
            "python-exec",
            "python-compile",
            "python-dynamic-import",
            "python-builtin-import",
            "python-cross-getattr",
        ],
    },
    LangEntry {
        name: "ruby",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::None,
        blind_kinds: &["rb-eval", "rb-instance-eval", "rb-send"],
    },
    LangEntry {
        name: "rust",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::Implemented,
        blind_kinds: &["rs-transmute-fn", "rs-libloading-get"],
    },
    LangEntry {
        name: "swift",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::None,
        blind_kinds: &["swift-nsclass-from-string", "swift-perform-selector"],
    },
    LangEntry {
        name: "typescript",
        blindspot_emitter: Status::Implemented,
        indirect_dispatch: Status::Implemented,
        blind_kinds: &[
            "ts-eval",
            "ts-function-ctor",
            "ts-dynamic-import",
            "ts-dynamic-require",
        ],
    },
];

pub fn run(args: SchemaArgs) -> Result<(), EcpError> {
    match args.command {
        SchemaCommands::Blindspots(a) => blindspots(a),
        SchemaCommands::Reltypes(a) => reltypes(a),
        SchemaCommands::NodeKinds(a) => node_kinds(a),
        SchemaCommands::GraphVersion(a) => graph_version(a),
    }
}

fn blindspots(args: FormatArgs) -> Result<(), EcpError> {
    let report = BlindspotsReport {
        languages: LANGUAGES,
    };
    emit(&args.format, &report, print_blindspots_text)
}

fn print_blindspots_text(report: &BlindspotsReport) {
    println!("lang             emitter   indirect  kinds");
    println!("--------------------------------------------------");
    for lang in report.languages {
        let emitter = match lang.blindspot_emitter {
            Status::Implemented => "yes",
            Status::None => "no",
        };
        let indirect = match lang.indirect_dispatch {
            Status::Implemented => "yes",
            Status::None => "no",
        };
        println!(
            "{:<16} {:<9} {:<9} {}",
            lang.name,
            emitter,
            indirect,
            lang.blind_kinds.join(", ")
        );
    }
}

/// Generic JSON/text dispatcher — keeps each subcommand's run-fn trivial.
fn emit<T: Serialize>(format: &str, payload: &T, text_printer: fn(&T)) -> Result<(), EcpError> {
    match format {
        "json" => {
            let s = serde_json::to_string_pretty(payload)
                .map_err(|e| EcpError::Serialization(format!("schema report: {e}")))?;
            println!("{}", s);
        }
        "text" => text_printer(payload),
        other => {
            return Err(EcpError::InvalidArgument(format!(
                "unknown --format `{}`; expected `json` or `text`",
                other
            )));
        }
    }
    Ok(())
}

// ── reltypes ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct RelTypeEntry {
    name: &'static str,
    /// LLM-utility category per CLAUDE.md graph-schema rule:
    /// `A` = graph completeness (without it `impact` lies);
    /// `B` = node coverage (LLM falls back to grep);
    /// `C` = edge semantics (ambiguous without distinct kind).
    utility: &'static str,
    /// Heuristic edges carry < 0.7 confidence and are filtered out of
    /// graph-completeness queries unless `--include-heuristic` is set.
    heuristic: bool,
    note: &'static str,
}

#[derive(Serialize)]
struct ReltypesReport {
    reltypes: &'static [RelTypeEntry],
}

/// 19 RelType variants. Order matches the `#[repr(u8)]` discriminant in
/// `ecp_core::graph::RelType` — kept aligned so the JSON output's index
/// matches the rkyv ordinal.
const RELTYPES: &[RelTypeEntry] = &[
    RelTypeEntry { name: "Defines", utility: "A", heuristic: false, note: "File/Namespace/Module → contained symbol. Drives scope-containment queries." },
    RelTypeEntry { name: "Imports", utility: "A", heuristic: false, note: "File → imported module/symbol. Source of cross-file resolution." },
    RelTypeEntry { name: "Calls", utility: "A", heuristic: false, note: "Caller → callee. Carries CallMeta flags for indirect dispatch." },
    RelTypeEntry { name: "Extends", utility: "A", heuristic: false, note: "Subclass/subtrait → base type. Class-level hierarchy edge." },
    RelTypeEntry { name: "Implements", utility: "A", heuristic: false, note: "Concrete type → interface/trait/protocol. Powers impl-enumeration for refactors." },
    RelTypeEntry { name: "HasMethod", utility: "A", heuristic: false, note: "Class/Trait/Struct → method. Container relationship for method-level dispatch." },
    RelTypeEntry { name: "HasProperty", utility: "A", heuristic: false, note: "Class/Struct → field/property. Schema-aware queries hang here." },
    RelTypeEntry { name: "Accesses", utility: "C", heuristic: false, note: "Reader/writer → variable / property. Distinct from Calls so reads are queryable." },
    RelTypeEntry { name: "HandlesRoute", utility: "A", heuristic: false, note: "Function/Method → Route. Materialised even for arrow / lambda handlers." },
    RelTypeEntry { name: "StepInProcess", utility: "B", heuristic: false, note: "Workflow / Saga step linkage. Process node ↔ step function." },
    RelTypeEntry { name: "References", utility: "C", heuristic: false, note: "Generic reference fallback (entry-point scoring, type-annotation linkage). Reason field carries provenance." },
    RelTypeEntry { name: "Fetches", utility: "A", heuristic: false, note: "HTTP-client call site → Route. URL-match across files; reason encodes accessed keys + per-file count." },
    RelTypeEntry { name: "MirrorsField", utility: "A", heuristic: true, note: "Heuristic ORM-field linkage from in-memory struct to SchemaField. Filtered unless --include-heuristic." },
    RelTypeEntry { name: "Publishes", utility: "A", heuristic: false, note: "Producer (kafka.send / SNS publish / RabbitMQ basicPublish) → EventTopic." },
    RelTypeEntry { name: "Subscribes", utility: "A", heuristic: false, note: "Consumer (@KafkaListener / SQS receive) → EventTopic. Pair with Publishes for cross-service event flow." },
    RelTypeEntry { name: "EventTopicMirror", utility: "B", heuristic: true, note: "Heuristic: EventTopic → SchemaField when payload shape inferable. Confidence < 0.85." },
    RelTypeEntry { name: "OpensTxScope", utility: "A", heuristic: false, note: "Reverse edge from TransactionScope back to opener Function/Method. Direction reads as 'scope's opener is X'." },
    RelTypeEntry { name: "Overrides", utility: "A", heuristic: false, note: "Method-level override (Java @Override, Kotlin override fun, C# override, C++ virtual-match). Distinct from class-level Extends." },
    RelTypeEntry { name: "Decorates", utility: "A", heuristic: false, note: "Decorator/attribute → decorated symbol (Python @decorator, Java/Kotlin @annotation, C# attribute, Rust attribute macro). 10-language emission." },
    RelTypeEntry { name: "UsesPathLiteral", utility: "A", heuristic: false, note: "Function/Method → PathLiteral. Drives `ecp impact --literal <value>` to find every read/write site touching a filesystem path or config key. 14-language emission." },
];

fn reltypes(args: FormatArgs) -> Result<(), EcpError> {
    let report = ReltypesReport { reltypes: RELTYPES };
    emit(&args.format, &report, print_reltypes_text)
}

fn print_reltypes_text(report: &ReltypesReport) {
    println!("name              utility  heuristic  note");
    println!("--------------------------------------------------------------------------------");
    for r in report.reltypes {
        println!(
            "{:<17} {:<8} {:<10} {}",
            r.name,
            r.utility,
            if r.heuristic { "yes" } else { "no" },
            r.note
        );
    }
}

// ── node-kinds ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct NodeKindEntry {
    name: &'static str,
    /// Functional category. Multiple variants per category is expected —
    /// the `distinction` field explains why they're not collapsed.
    category: &'static str,
    /// Why this variant is NOT folded into a sibling. Empty when the variant
    /// is unambiguous (e.g. `File`).
    distinction: &'static str,
}

#[derive(Serialize)]
struct NodeKindsReport {
    node_kinds: &'static [NodeKindEntry],
    /// `NodeKind::VARIANT_COUNT` mirror — pinned so consumers can assert
    /// against `ecp_core::graph::NodeKind::VARIANT_COUNT` without parsing
    /// the array length.
    variant_count: usize,
}

/// 27 NodeKind variants. Order matches `#[repr(u8)]` discriminant.
const NODE_KINDS: &[NodeKindEntry] = &[
    NodeKindEntry { name: "File", category: "structural", distinction: "" },
    NodeKindEntry { name: "Function", category: "callable", distinction: "Top-level fn; distinct from Method (member dispatch) and Constructor (init)." },
    NodeKindEntry { name: "Class", category: "type", distinction: "Reference type with vtable + inheritance. NOT Struct (value-type, no vtable)." },
    NodeKindEntry { name: "Method", category: "callable", distinction: "Member function on Class/Struct/Trait. Receiver-bound dispatch." },
    NodeKindEntry { name: "Interface", category: "type", distinction: "Java/C# style. Distinct from Trait (Rust/Scala — different default-method + dispatch semantics)." },
    NodeKindEntry { name: "Constructor", category: "callable", distinction: "Init dispatch site. Distinct from Method so call-graph queries can filter init vs invoke." },
    NodeKindEntry { name: "Property", category: "data", distinction: "In-memory member. Distinct from SchemaField (DB-backed) to avoid migration-safety false hits." },
    NodeKindEntry { name: "Variable", category: "data", distinction: "Local / module-level binding. Not for in-class fields (that's Property)." },
    NodeKindEntry { name: "Const", category: "data", distinction: "Compile-time constant binding. Distinct from Variable (mutable) and EventTopic (routing semantics)." },
    NodeKindEntry { name: "Import", category: "structural", distinction: "Import statement / use binding. Drives Imports edges." },
    NodeKindEntry { name: "Route", category: "framework", distinction: "HTTP-route declaration. Carries RouteShape (response keys) for shape-check verdicts." },
    NodeKindEntry { name: "Process", category: "framework", distinction: "Saga / workflow / orchestrator step container." },
    NodeKindEntry { name: "Document", category: "doc", distinction: "Markdown / RST document node — links to embedded code blocks." },
    NodeKindEntry { name: "Section", category: "doc", distinction: "Sub-region inside a Document (heading + body). Discounted in symbol density." },
    NodeKindEntry { name: "EntryPoint", category: "scored", distinction: "Cross-lang entry-point marker. References underlying Function/Method via References edge with score in `reason`." },
    NodeKindEntry { name: "Struct", category: "type", distinction: "C / Rust / Swift value-type. NOT Class — no vtable, value-copy, no inheritance for C." },
    NodeKindEntry { name: "Enum", category: "type", distinction: "Discriminated union. Distinct from Class so LLMs don't pattern-match OO conventions." },
    NodeKindEntry { name: "Typedef", category: "type", distinction: "Pure forwarding alias (C typedef, Rust `type X = Y`, TS `type X = ...`). No member surface." },
    NodeKindEntry { name: "Namespace", category: "structural", distinction: "C# / PHP / C++ lexical scope container. Drives qualifier resolution (Tier 2.5)." },
    NodeKindEntry { name: "Module", category: "structural", distinction: "Rust `mod`, Python file-as-module, Kotlin `package`. Drives import resolution." },
    NodeKindEntry { name: "Macro", category: "callable", distinction: "C/C++ `#define`. Textual expansion — different binding semantics from Function." },
    NodeKindEntry { name: "Annotation", category: "metadata", distinction: "Java/Kotlin @interface, C# attribute class. Distinct from Decorator (call-site annotation) — this is the *definition*." },
    NodeKindEntry { name: "Trait", category: "type", distinction: "Rust trait, PHP trait, Swift protocol, Scala trait. Distinct from Interface (different dispatch + default-method semantics)." },
    NodeKindEntry { name: "Impl", category: "structural", distinction: "Rust `impl` block. Associates methods with a concrete type — not a directly-callable symbol but needed by inspect." },
    NodeKindEntry { name: "SchemaField", category: "data", distinction: "DB column / ORM model field. Distinct from Property so migration-drift queries don't false-hit in-memory fields." },
    NodeKindEntry { name: "EventTopic", category: "framework", distinction: "Kafka topic / SNS topic / EventBridge rule. Carries routing semantics — distinct from Const." },
    NodeKindEntry { name: "TransactionScope", category: "framework", distinction: "Transaction boundary (@Transactional, BEGIN…COMMIT). Distinct from Function so atomicity queries resolve at the right granularity." },
    NodeKindEntry { name: "EnumVariant", category: "type", distinction: "Individual case/member of an Enum (TS enum member, Rust enum variant, Java/Kotlin enum constant, Swift case). 8-language emission. Distinct from Property because it belongs to the Enum's discriminant set, not the in-memory field set." },
    NodeKindEntry { name: "PathLiteral", category: "data", distinction: "String literal that names a filesystem path or config key (14-language coverage). Distinct from Const because the value is a path/key referenced by `UsesPathLiteral` edges — drives `ecp impact --literal <value>` queries to find every read/write site." },
];

fn node_kinds(args: FormatArgs) -> Result<(), EcpError> {
    let report = NodeKindsReport {
        node_kinds: NODE_KINDS,
        variant_count: ecp_core::graph::NodeKind::VARIANT_COUNT,
    };
    emit(&args.format, &report, print_node_kinds_text)
}

fn print_node_kinds_text(report: &NodeKindsReport) {
    println!("name              category    distinction");
    println!("--------------------------------------------------------------------------------");
    for n in report.node_kinds {
        println!("{:<17} {:<11} {}", n.name, n.category, n.distinction);
    }
    println!();
    println!("variant_count: {}", report.variant_count);
}

// ── graph-version ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct VersionBumpEntry {
    version: u32,
    reason: &'static str,
}

#[derive(Serialize)]
struct GraphVersionReport {
    current_version: u32,
    history: &'static [VersionBumpEntry],
}

/// Append-only schema-version log. New entry per `graph.bin`-affecting
/// schema change. Older versions aren't readable by the current binary —
/// `auto_ensure::ensure_fresh` triggers reindex on mismatch.
const VERSION_HISTORY: &[VersionBumpEntry] = &[
    VersionBumpEntry { version: 10, reason: "v10 baseline: kind_offsets CSR + StringPool layout for fast NodeKind/RelType slice access." },
    VersionBumpEntry { version: 11, reason: "Dense node_flags side table for O(1) FunctionMeta boolean flag filters." },
    VersionBumpEntry { version: 12, reason: "RawNode.field_reads + ReadsField RelType: function/method → field-read edges for impact across 14 languages." },
    // Future bumps append here. The is_test field on BlindSpotRecord
    // (FU-001) was added without a discriminant bump — rkyv tolerates the
    // trailing-field addition as long as nothing earlier shifts.
];

fn graph_version(args: FormatArgs) -> Result<(), EcpError> {
    let report = GraphVersionReport {
        current_version: ecp_core::graph::GRAPH_FORMAT_VERSION,
        history: VERSION_HISTORY,
    };
    emit(&args.format, &report, print_graph_version_text)
}

fn print_graph_version_text(report: &GraphVersionReport) {
    println!("current_version: {}", report.current_version);
    println!();
    println!("version  reason");
    println!("------------------------------------------------------------");
    for e in report.history {
        println!("v{:<7} {}", e.version, e.reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_14_mainstream_languages_listed() {
        // CLAUDE.md parser change rule: 14 mainstream langs must have
        // BlindSpot coverage. This test pins the schema-cmd output to the
        // same set so dropping a lang here triggers a CI failure.
        let names: Vec<&str> = LANGUAGES.iter().map(|l| l.name).collect();
        for expected in &[
            "typescript",
            "javascript",
            "python",
            "java",
            "kotlin",
            "c_sharp",
            "go",
            "rust",
            "php",
            "ruby",
            "swift",
            "c",
            "cpp",
            "dart",
        ] {
            assert!(
                names.contains(expected),
                "missing language `{}` in schema-cmd inventory",
                expected
            );
        }
    }

    #[test]
    fn every_language_has_at_least_one_blind_kind() {
        for lang in LANGUAGES {
            assert!(
                !lang.blind_kinds.is_empty(),
                "language `{}` has empty blind_kinds — either remove it or add an emitter",
                lang.name
            );
        }
    }

    #[test]
    fn blind_kinds_are_unique_globally() {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for lang in LANGUAGES {
            for kind in lang.blind_kinds {
                assert!(
                    seen.insert(kind),
                    "duplicate blind kind `{}` across languages",
                    kind
                );
            }
        }
    }

    // ── reltypes invariants ─────────────────────────────────────────────

    #[test]
    fn reltypes_inventory_count_matches_repr_u8_enum() {
        // RelType currently has 20 variants (post-merge: `Decorates` from
        // #365 + `UsesPathLiteral` from #367). If a new variant lands,
        // bump this number AND append the entry to RELTYPES. RelType has
        // no VARIANT_COUNT constant like NodeKind does — manual sync.
        assert_eq!(
            RELTYPES.len(),
            20,
            "reltypes inventory drifted from RelType enum"
        );
    }

    #[test]
    fn every_reltype_has_a_note_and_valid_utility() {
        for r in RELTYPES {
            assert!(!r.note.is_empty(), "reltype `{}` missing note", r.name);
            assert!(
                matches!(r.utility, "A" | "B" | "C"),
                "reltype `{}` utility must be A/B/C, got `{}`",
                r.name,
                r.utility
            );
        }
    }

    #[test]
    fn heuristic_reltypes_are_the_two_documented_ones() {
        let heuristics: Vec<&str> = RELTYPES
            .iter()
            .filter(|r| r.heuristic)
            .map(|r| r.name)
            .collect();
        // RelType::is_heuristic is hard-coded for MirrorsField + EventTopicMirror;
        // pin the schema-cmd output to the same pair so drift surfaces.
        assert_eq!(heuristics, vec!["MirrorsField", "EventTopicMirror"]);
    }

    // ── node-kinds invariants ───────────────────────────────────────────

    #[test]
    fn node_kinds_inventory_matches_variant_count_const() {
        assert_eq!(
            NODE_KINDS.len(),
            ecp_core::graph::NodeKind::VARIANT_COUNT,
            "NODE_KINDS array length drifted from NodeKind::VARIANT_COUNT"
        );
    }

    #[test]
    fn collision_pairs_are_documented_in_distinction() {
        // CLAUDE.md flags these distinctions as load-bearing for LLM accuracy.
        // The distinction field must call them out, otherwise the schema-cmd
        // output gives an LLM no reason to pick one over the other.
        let by_name: std::collections::HashMap<&str, &NodeKindEntry> =
            NODE_KINDS.iter().map(|n| (n.name, n)).collect();
        let pairs: &[(&str, &str)] = &[
            ("Struct", "Class"),
            ("Trait", "Interface"),
            ("Enum", "Class"),
            ("SchemaField", "Property"),
            ("EventTopic", "Const"),
            ("TransactionScope", "Function"),
        ];
        for (a, b) in pairs {
            let entry = by_name.get(a).unwrap_or_else(|| panic!("`{a}` missing"));
            assert!(
                entry.distinction.contains(b),
                "`{a}` distinction must mention `{b}` (got: `{}`)",
                entry.distinction
            );
        }
    }

    // ── graph-version invariants ────────────────────────────────────────

    #[test]
    fn graph_version_history_includes_current_version() {
        let current = ecp_core::graph::GRAPH_FORMAT_VERSION;
        assert!(
            VERSION_HISTORY.iter().any(|e| e.version == current),
            "current GRAPH_FORMAT_VERSION ({current}) missing from VERSION_HISTORY"
        );
    }

    #[test]
    fn version_history_is_monotonic_non_decreasing() {
        let mut prev = 0u32;
        for e in VERSION_HISTORY {
            assert!(
                e.version >= prev,
                "VERSION_HISTORY must be monotonic non-decreasing; v{} after v{prev}",
                e.version
            );
            prev = e.version;
        }
    }
}
