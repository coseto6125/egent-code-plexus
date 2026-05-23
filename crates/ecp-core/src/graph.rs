use crate::pool::StrRef;
use rkyv::{Archive, Deserialize, Serialize};

/// Magic bytes at the head of every `graph.bin`. Used by the reader to
/// reject non-ecp files (or files truncated below the header length)
/// before rkyv attempts a structural cast.
pub const GRAPH_MAGIC: [u8; 8] = *b"ECP-RS\0\0";

/// On-disk graph format version. Bump whenever `ZeroCopyGraph`'s field
/// layout changes in a way that would make older binaries unreadable by
/// the new reader (or vice-versa). The reader refuses any version it
/// does not recognize, so a stale CLI does not segfault on a fresh
/// `graph.bin` and a fresh CLI does not silently misinterpret old data.
///
/// v6: `Node.owner_class: StrRef` added for method rename isolation (T1-11).
/// v7: `Node.uid: StrRef` → `u64` (xxh3_64 streaming hash, T1-5).
///     Canonical bytes: `kind_as_str \0 path \0 owner_class_or_empty \0 name`.
///     1-cycle `FxHashMap` lookup; eliminates string-pool dereference on hot paths.
/// v8: `Node.content_hash: u64` added — per-symbol xxh3_64 of raw source bytes
///     for T7-4/5/6 incremental skip (equal hash ↔ body unchanged).
pub const GRAPH_FORMAT_VERSION: u32 = 10;

impl std::str::FromStr for NodeKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "file" => Ok(NodeKind::File),
            "function" => Ok(NodeKind::Function),
            "class" => Ok(NodeKind::Class),
            "method" => Ok(NodeKind::Method),
            "interface" => Ok(NodeKind::Interface),
            "constructor" => Ok(NodeKind::Constructor),
            "property" => Ok(NodeKind::Property),
            "variable" => Ok(NodeKind::Variable),
            "const" => Ok(NodeKind::Const),
            "import" => Ok(NodeKind::Import),
            "route" => Ok(NodeKind::Route),
            "process" => Ok(NodeKind::Process),
            "document" => Ok(NodeKind::Document),
            "section" => Ok(NodeKind::Section),
            "entrypoint" | "entry_point" | "entry point" => Ok(NodeKind::EntryPoint),
            "struct" => Ok(NodeKind::Struct),
            "enum" => Ok(NodeKind::Enum),
            "typedef" | "typealias" | "type_alias" | "type alias" => Ok(NodeKind::Typedef),
            "namespace" => Ok(NodeKind::Namespace),
            "module" | "mod" => Ok(NodeKind::Module),
            "macro" => Ok(NodeKind::Macro),
            "annotation" => Ok(NodeKind::Annotation),
            "trait" | "protocol" => Ok(NodeKind::Trait),
            "impl" => Ok(NodeKind::Impl),
            "schemafield" | "schema_field" | "schema field" => Ok(NodeKind::SchemaField),
            "eventtopic" | "event_topic" | "event topic" => Ok(NodeKind::EventTopic),
            "transactionscope" | "transaction_scope" | "transaction scope" => {
                Ok(NodeKind::TransactionScope)
            }
            "enumvariant" | "enum_variant" | "enum variant" => Ok(NodeKind::EnumVariant),
            _ => Err(()),
        }
    }
}

impl std::str::FromStr for RelType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "CALLS" => Ok(RelType::Calls),
            "EXTENDS" => Ok(RelType::Extends),
            "IMPORTS" => Ok(RelType::Imports),
            "IMPLEMENTS" => Ok(RelType::Implements),
            "HAS_METHOD" => Ok(RelType::HasMethod),
            "HAS_PROPERTY" => Ok(RelType::HasProperty),
            "ACCESSES" => Ok(RelType::Accesses),
            "HANDLES_ROUTE" => Ok(RelType::HandlesRoute),
            "STEP_IN_PROCESS" => Ok(RelType::StepInProcess),
            "REFERENCES" => Ok(RelType::References),
            "DEFINES" => Ok(RelType::Defines),
            "FETCHES" => Ok(RelType::Fetches),
            "MIRRORSFIELD" | "MIRRORS_FIELD" => Ok(RelType::MirrorsField),
            "PUBLISHES" => Ok(RelType::Publishes),
            "SUBSCRIBES" => Ok(RelType::Subscribes),
            "EVENTTOPICMIRROR" | "EVENT_TOPIC_MIRROR" => Ok(RelType::EventTopicMirror),
            "OPENSTXSCOPE" | "OPENS_TX_SCOPE" => Ok(RelType::OpensTxScope),
            "OVERRIDES" => Ok(RelType::Overrides),
            "DECORATES" => Ok(RelType::Decorates),
            _ => Err(()),
        }
    }
}

impl RelType {
    pub const fn is_heuristic(self) -> bool {
        matches!(self, Self::MirrorsField | Self::EventTopicMirror)
    }

    /// Static variant name. Mirrors `NodeKind::as_str` so cypher's
    /// `type(r)` scalar and `graph_query`'s rel-filter share one rendering.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Defines => "Defines",
            Self::Imports => "Imports",
            Self::Calls => "Calls",
            Self::Extends => "Extends",
            Self::Implements => "Implements",
            Self::HasMethod => "HasMethod",
            Self::HasProperty => "HasProperty",
            Self::Accesses => "Accesses",
            Self::HandlesRoute => "HandlesRoute",
            Self::StepInProcess => "StepInProcess",
            Self::References => "References",
            Self::Fetches => "Fetches",
            Self::MirrorsField => "MirrorsField",
            Self::Publishes => "Publishes",
            Self::Subscribes => "Subscribes",
            Self::EventTopicMirror => "EventTopicMirror",
            Self::OpensTxScope => "OpensTxScope",
            Self::Overrides => "Overrides",
            Self::Decorates => "Decorates",
        }
    }
}

impl From<&ArchivedRelType> for RelType {
    /// Zero-copy conversion — same pattern as `From<&ArchivedNodeKind> for
    /// NodeKind` above. Both are `#[repr(u8)]` with identical discriminant
    /// layout; reading via raw pointer avoids the 18-arm match that compounds
    /// the SIGSEGV risk documented on the NodeKind impl.
    ///
    /// SAFETY: `ArchivedRelType` is rkyv's archive of a `#[repr(u8)]` enum
    /// with the same discriminants as `RelType`. Bad discriminant byte only
    /// arises from a malformed `graph.bin` that already failed `validate_header`.
    fn from(a: &ArchivedRelType) -> Self {
        unsafe { std::ptr::read(a as *const ArchivedRelType as *const RelType) }
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum NodeKind {
    #[default]
    File,
    Function,
    Class,
    Method,
    Interface,
    Constructor,
    Property,
    Variable,
    Const,
    Import,
    Route,
    Process,
    Document,
    Section,
    /// Scored entry-point marker — emitted by the cross-language scorer in
    /// `ecp_analyzer::entry_points`. References the underlying
    /// handler (`Function` / `Method` / `Route`) via a `References` edge;
    /// the edge's `reason` carries the scoring provenance (e.g. `main:0.9`,
    /// `route:1.0`, `framework_ref:0.8`).
    EntryPoint,
    // ── Parity expansion (parity-14-langs) ──────────────────────────────
    // Appended at the END to keep rkyv discriminants stable for existing
    // graph.bin files. Variants prioritised by LLM-utility delta vs the
    // previous `Class` / `Interface` / `Function` catch-alls (see
    // `scripts/parity/symbol_diffs/summary.md`).
    /// Value-type aggregate: C `struct`, Rust `struct`, Swift `struct`,
    /// Dart `class` with value-semantics. Distinct from `Class` because
    /// runtime semantics differ (no vtable, value-copy, no inheritance for C).
    Struct,
    /// Discriminated union / sum type: Rust `enum`, Swift `enum`, Java
    /// `enum`, C# `enum`, TS `enum`. Distinguished from `Class` so LLMs
    /// don't pattern-match against OO conventions.
    Enum,
    /// Pure type alias with no member surface: C `typedef`, Rust
    /// `type X = Y`, Swift `typealias`, TS `type X = ...`. Lookups treat
    /// it as a forwarding pointer.
    Typedef,
    /// Lexical scope container: C# `namespace`, PHP `namespace`,
    /// C++ `namespace`. Holds qualifier-resolution context.
    Namespace,
    /// Mod-tree node: Rust `mod foo`, Python file-as-module, Kotlin
    /// `package`. Drives import resolution.
    Module,
    /// Preprocessor symbol: C/C++ `#define`. Different binding semantics
    /// from `Function` / `Const` because expansion is textual.
    Macro,
    /// Declarative metadata: Java/Kotlin `@interface` and `annotation
    /// class`, C# attributes. Drives framework behavior detection.
    Annotation,
    /// Protocol/trait: Rust `trait`, PHP `trait`, Swift `protocol`,
    /// Scala `trait`. Distinct from `Interface` (Java/C#) — different
    /// dispatch + default-method semantics.
    Trait,
    /// Rust `impl` block: associates methods with a concrete type. Not
    /// a symbol callers reach for directly, but `ecp inspect` needs it
    /// to enumerate associated functions per type.
    Impl,
    // ── Schema / event / transaction expansion ─────────────────────────
    // Appended at the END to keep rkyv discriminants stable. Variants
    // address data-layer and event-driven patterns that previously collapsed
    // into `Property` / `Variable` / `Function` and obscured cross-service
    // contracts for LLM queries.
    /// Named column / field in a database schema or ORM model: Django
    /// `models.Field`, SQLAlchemy `Column`, Prisma `@field`, Rust
    /// `sqlx::FromRow` member. Distinct from `Property` so LLM queries
    /// about schema drift and migration safety resolve without false hits
    /// on in-memory object fields.
    SchemaField,
    /// Named pub/sub topic or event type: Kafka topic, SNS topic, EventBridge
    /// rule, RabbitMQ queue. Distinct from `Const` because it carries routing
    /// semantics — `Publishes` / `Subscribes` edges make producer/consumer
    /// graphs queryable without parsing string literals.
    EventTopic,
    /// Database transaction boundary: `@Transactional`, `BEGIN…COMMIT` block,
    /// SQLAlchemy `Session.begin()`. Distinct from `Function` so LLM queries
    /// about atomicity scope and rollback paths resolve at the right
    /// granularity without scanning all function bodies.
    TransactionScope,
    // ── Enum variant expansion ──────────────────────────────────────────
    // Appended at the END to keep rkyv discriminants stable.
    /// Single member of an enum (Rust `enum X { A }`, TS `enum X { A }`,
    /// Java/Kotlin/Swift `enum X { case A }`, C# `enum X { A = 5 }`). Carries
    /// the variant's name; payload shape (associated values / data carriers)
    /// not modeled — query via `(v.content)` for the source if needed.
    ///
    /// LLM-utility filter: (B) Node coverage — without this variant, state
    /// machine queries and exhaustiveness analysis fall back to grep.
    /// Enables:
    ///   `MATCH (e:Enum {name:"Status"})-[:Defines]->(v:EnumVariant) RETURN v`
    ///   `ecp impact Active --upstream` to find all variant consumers
    EnumVariant,
}

impl NodeKind {
    /// True when the node represents an invokable target (CALLS edge sink).
    pub const fn is_callable(self) -> bool {
        matches!(self, Self::Function | Self::Method | Self::Constructor)
    }

    /// True when the node represents an extendable / type-binding target
    /// (EXTENDS edges, type annotations). Includes the parity-14-langs
    /// value-type variants so Rust `struct Foo` / `type Foo = Bar` / `trait
    /// Foo` remain reachable by the qualifier-scoped resolver (Tier 2.5).
    pub const fn is_type(self) -> bool {
        matches!(
            self,
            Self::Class | Self::Interface | Self::Struct | Self::Enum | Self::Typedef | Self::Trait
        )
    }

    /// True when the node can appear as the leading segment of a qualified
    /// callee (`outer::member()` / `outer.member()`). Superset of `is_type`
    /// plus `Namespace` (C++ / C# / PHP) and `Module` (Rust inline
    /// `mod foo { ... }`). Without these, every qualified call where the
    /// leading segment isn't a class / struct / enum / typedef / trait /
    /// interface drops at Tier 2.5 and falls through to the bare-name
    /// global tier — which then rejects ultra-common member names like
    /// `new` / `default` / `bar`.
    ///
    /// Rust note: file-backed `mod foo;` still resolves via the
    /// language-specific Tier 3.5 (module-tree FQN) / Tier 4 (module-file
    /// fallback) paths. Inline modules have no backing file, so they
    /// previously had no path at all — `Module` here closes that gap.
    pub const fn is_qualifier(self) -> bool {
        self.is_type() || matches!(self, Self::Namespace | Self::Module)
    }

    /// Number of `NodeKind` variants. Used to size the v10 `kind_offsets`
    /// CSR array (`length = VARIANT_COUNT + 1`). Append-only schema rule
    /// means this only ever grows; matching the variant total at the bottom
    /// of the enum keeps the CI green when a new kind lands.
    pub const VARIANT_COUNT: usize = 28;

    /// Discriminant as a usize, suitable for indexing into the v10
    /// `kind_offsets` array. Matches the `#[repr(u8)]` order so the
    /// CSR index `kind_offsets[k.as_index()..k.as_index()+1]` is the
    /// half-open range of node indices for kind `k`.
    pub const fn as_index(self) -> usize {
        self as u8 as usize
    }

    /// Static variant name. Used by `ecp_core::uid::compute` as the first
    /// segment of the canonical byte stream fed to xxh3-64.
    /// Must match the variant identifier exactly for byte-stable hashes.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Function => "Function",
            Self::Class => "Class",
            Self::Method => "Method",
            Self::Interface => "Interface",
            Self::Constructor => "Constructor",
            Self::Property => "Property",
            Self::Variable => "Variable",
            Self::Const => "Const",
            Self::Import => "Import",
            Self::Route => "Route",
            Self::Process => "Process",
            Self::Document => "Document",
            Self::Section => "Section",
            Self::EntryPoint => "EntryPoint",
            Self::Struct => "Struct",
            Self::Enum => "Enum",
            Self::Typedef => "Typedef",
            Self::Namespace => "Namespace",
            Self::Module => "Module",
            Self::Macro => "Macro",
            Self::Annotation => "Annotation",
            Self::Trait => "Trait",
            Self::Impl => "Impl",
            Self::SchemaField => "SchemaField",
            Self::EventTopic => "EventTopic",
            Self::TransactionScope => "TransactionScope",
            Self::EnumVariant => "EnumVariant",
        }
    }
}

impl From<&ArchivedNodeKind> for NodeKind {
    /// Zero-copy conversion: both types are `#[repr(u8)]` with identical
    /// discriminant layout. Reading via raw pointer avoids a 27-arm match
    /// expression that previously triggered a rustc `check_unsafety` SIGSEGV
    /// on stable when compiled in the same crate as `ZeroCopyGraph`.
    ///
    /// SAFETY: `ArchivedNodeKind` is rkyv's archive of a `#[repr(u8)]` enum
    /// with the same discriminants as `NodeKind`. A bad discriminant byte only
    /// arises from a malformed `graph.bin` that already failed the magic +
    /// version guard in `validate_header()`.
    fn from(a: &ArchivedNodeKind) -> Self {
        // SAFETY: see doc-comment above.
        unsafe { std::ptr::read(a as *const ArchivedNodeKind as *const NodeKind) }
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum RelType {
    Defines,
    Imports,
    Calls,
    Extends,
    Implements,
    HasMethod,
    HasProperty,
    Accesses,
    HandlesRoute,
    StepInProcess,
    References,
    /// HTTP client → Route edge: a consumer file calls `fetch(...)` /
    /// `axios.get(...)` against a URL that matches a Route handler. The
    /// `Edge.reason` encodes accessed response keys + per-file fetch
    /// count as `fetch-url-match[|keys:a,b][|fetches:N]`, parsed by
    /// `ecp_analyzer::fetch_shape`.
    Fetches,
    // ── Schema / event / transaction expansion ─────────────────────────
    // Appended at the END to keep rkyv discriminants stable.
    /// Heuristic: in-memory struct field → `SchemaField` when the struct
    /// derives an ORM trait. Low-confidence — verified by `is_heuristic()`.
    MirrorsField,
    /// Producer → `EventTopic`: the source node emits events to this topic
    /// (e.g. `kafka.send(TOPIC, ...)`, SNS `publish`).
    Publishes,
    /// Consumer → `EventTopic`: the source node consumes events from this
    /// topic (e.g. `@KafkaListener(topics=TOPIC)`).
    Subscribes,
    /// Heuristic: `EventTopic` → `SchemaField` mirroring the event payload
    /// schema. Low-confidence — verified by `is_heuristic()`.
    EventTopicMirror,
    /// Reverse-direction edge from a `TransactionScope` back to the
    /// `Function` / `Method` that opens or manages it. Read as
    /// "scope's opener is X" — the name follows the *relation*, not the
    /// edge direction, so a single CSR slice from the scope answers
    /// "who opens this scope?" without a join.
    OpensTxScope,
    /// Method-level override edge. Source is a concrete method
    /// (`Function` / `Method` / `Constructor`) on a subtype; target is the
    /// corresponding method on the *immediate* supertype or interface that the
    /// source overrides. Distinct from class-level `Extends` / `Implements`
    /// — those link `Class`/`Interface` nodes, while `Overrides` links method
    /// nodes. LLM-utility (A) Graph completeness: refactoring a base method
    /// must find every overriding implementation; without this edge the only
    /// option is grep-and-pray. (C) Edge semantics: `Extends` carries a
    /// different meaning (type hierarchy) and cannot substitute.
    ///
    /// Target is the **immediate** supertype's method, not a transitive
    /// ancestor. For `C extends B extends A; A.foo; B.foo; C.foo` the edges
    /// are `C.foo → B.foo` and `B.foo → A.foo`; querying the full chain
    /// requires two hops, which is the correct semantic (C overrides B's
    /// contract, not A's directly).
    ///
    /// Languages: Java (`@Override`), Kotlin (`override fun`), C# (`override`
    /// modifier), C++ (`override` specifier or virtual-matched signature).
    /// Appended at the END to preserve rkyv discriminants for existing
    /// `graph.bin` files.
    Overrides,
    /// Edge from a decorated symbol (Class / Function / Method / Property /
    /// Constructor) to an `Annotation` node OR a resolved annotation class.
    ///
    /// LLM-utility filter (C) Edge semantics: decorator-driven dispatch
    /// (Spring `@Injectable`, Hilt DI, Django `@receiver`, Rust `#[derive]`,
    /// C# `[Authorize]`) is invisible to graph queries without this edge;
    /// refactors must use grep + manual cross-reference. With this edge:
    ///   `MATCH (c:Class)-[:Decorates]->(a:Annotation {name:"Injectable"}) RETURN c`
    ///   `ecp impact <annotation>` traverses all use sites directly.
    ///
    /// Resolution: decorator names are resolved via the same `Resolver` used
    /// by Calls/Implements. On resolver hit the edge targets the resolved
    /// class. On miss a synthetic `NodeKind::Annotation` node (deduped per
    /// name across the whole graph) is emitted as the target.
    ///
    /// Appended at the END to preserve rkyv discriminants for existing
    /// `graph.bin` files.
    Decorates,
}

impl ArchivedRelType {
    /// Mirror of `RelType::is_heuristic` for zero-copy graph traversal.
    pub const fn is_heuristic(&self) -> bool {
        matches!(self, Self::MirrorsField | Self::EventTopicMirror)
    }

    /// Structural containment edges describe "where a symbol lives", not
    /// "who calls or reaches it". Upstream / downstream impact BFS must
    /// exclude these so File→Function Defines edges don't register as callers.
    pub const fn is_scope_containment(&self) -> bool {
        matches!(
            self,
            Self::Defines | Self::HasMethod | Self::HasProperty | Self::Imports
        )
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Default)]
#[rkyv(derive(Debug))]
pub struct Node {
    /// Deterministic xxh3-64 hash of the canonical byte stream:
    /// `kind_as_str \0 path \0 owner_class_or_empty \0 name`.
    /// Computed by `ecp_core::uid::compute`. Enables 1-cycle `FxHashMap`
    /// lookup in the resolver; eliminates string-pool dereference.
    pub uid: u64,
    pub name: StrRef,
    pub file_idx: u32,
    pub kind: NodeKind,
    pub span: (u32, u32, u32, u32), // start_line, start_col, end_line, end_col
    pub community_id: u16,          // 0 = unassigned
    /// Owning class/struct for methods and properties; `StrRef::default()` (len=0)
    /// means module-level symbol with no owner.  Appended at the END to preserve
    /// rkyv binary layout for v5 fields; format version bumped to 6.
    /// Used by `ecp rename` to isolate `Foo.validate` from `Bar.validate` (T1-11).
    pub owner_class: StrRef,
    /// xxh3-64 hash of the symbol's raw source bytes. Promoted from
    /// `RawNode.content_hash` by the builder. `0` for synthetic nodes
    /// (routes, entry-points, delegate stubs) with no source span.
    /// Enables T7-4/5/6 incremental skip: equal hash ↔ body unchanged.
    pub content_hash: u64,
}

/// Sorted entry in `ZeroCopyGraph.name_index`: maps a `xxh3_64(name)` hash
/// to the node index. The builder writes entries sorted by `name_hash`,
/// enabling O(log N) binary-search lookup of "all nodes with this name".
/// Hash collisions disambiguate by re-resolving the actual name from the
/// string pool and string-comparing.
///
/// Practical impact: `ecp find <name>`, `ecp rename <old> <new>` (collision
/// detection), `ecp inspect <name>`, and cypher `MATCH {name: "..."}`
/// previously did O(N) linear scans through `graph.nodes`. With this index
/// they become O(log N + collision_count).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct NameIndexEntry {
    pub name_hash: u64,
    pub node_idx: u32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct Edge {
    pub source: u32,
    pub target: u32,
    pub rel_type: RelType,
    pub confidence: f32,
    pub reason: StrRef,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
pub enum FileCategory {
    Source,
    Test,
    Reference,
    Document,
    Config,
    /// Framework example / sample / demo app. Distinct from `Test` because
    /// examples are canonical "how to use this framework" content that LLM
    /// queries (and route surfaces) should reach, while tests are meta —
    /// they test other code. Splitting unblocks route/tool emission for
    /// `/examples/` / `/sample/` / `/demo/` dirs that the historical
    /// `is_test` blanket-skipped. Appended at the END to preserve rkyv
    /// discriminants for existing graph.bin files.
    Example,
}

impl From<&ArchivedFileCategory> for FileCategory {
    fn from(a: &ArchivedFileCategory) -> Self {
        match a {
            ArchivedFileCategory::Source => FileCategory::Source,
            ArchivedFileCategory::Test => FileCategory::Test,
            ArchivedFileCategory::Reference => FileCategory::Reference,
            ArchivedFileCategory::Document => FileCategory::Document,
            ArchivedFileCategory::Config => FileCategory::Config,
            ArchivedFileCategory::Example => FileCategory::Example,
        }
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct File {
    pub path: StrRef,
    pub mtime: u64,
    pub content_hash: [u8; 8],
    pub category: FileCategory,
}

/// Per-Route response shape extracted from the handler's source. `node_idx`
/// points into `ZeroCopyGraph.nodes`. `response_keys` are the top-level
/// keys emitted on success paths (status 2xx or no status decoration);
/// `error_keys` are the keys emitted on 4xx/5xx paths. `shape_check` uses
/// `(response_keys ∪ error_keys)` as the "known" set against which
/// consumer-side accessed keys are compared.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RouteShape {
    pub node_idx: u32,
    pub response_keys: Vec<StrRef>,
    pub error_keys: Vec<StrRef>,
}

/// File-level record of a truly unresolvable code pattern (eval/dynamic
/// import/cross-object reflection/...). Persisted in the graph so that
/// `ecp context` / `ecp analyze` can surface blind spots to the LLM,
/// telling it "we cannot see past this site — confirm manually".
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct BlindSpotRecord {
    pub kind: StrRef,
    pub file_path: StrRef,
    pub start_row: u32,
    pub start_col: u32,
    pub end_row: u32,
    pub end_col: u32,
    pub hint: StrRef,
}

/// BlindSpot kind emitted by `resolution::builder::classify_collision` for
/// uid hash collisions. Exposed as a const so consumers
/// (`ecp dev uid-audit`) match against it without re-typing the literal.
pub const BS_KIND_UID_COLLISION: &str = "uid-collision";

/// Type-2 "parser-metric" BlindSpot kinds — emitted by the resolver's
/// `classify_collision` for uid hash collisions, overloaded methods, and
/// `#ifdef` redefinitions. These are parser-maintainer signal, NOT
/// LLM-actionable source-code opacity (which is Type-1: `python-eval`,
/// `python-exec`, `dynamic-import`, etc.).
///
/// `ecp summary.blind_spots` filters these out so its LLM-facing surface
/// stays focused; `ecp dev uid-audit` inspects them.
pub const DEV_METRIC_BS_KINDS: &[&str] = &[BS_KIND_UID_COLLISION, "method-overload", "ifdef-redef"];

/// True iff `kind` is one of [`DEV_METRIC_BS_KINDS`].
pub fn is_dev_metric_bs_kind(kind: &str) -> bool {
    DEV_METRIC_BS_KINDS.contains(&kind)
}

/// Field separator inside a uid-collision BlindSpot `hint` string.
///
/// ASCII Unit Separator (U+001F) — chosen because legitimate source-code
/// identifiers may contain `:` (Swift selectors `init(foo:bar:)`, Rust
/// path syntax `Mod::Type`, Windows paths `C:\…`), so colon-delimited
/// fields would be ambiguous to round-trip parse.
///
/// Producer: [`crate::graph::format_hint`] / `resolution::builder::classify_collision`.
/// Consumer: `ecp_cli::commands::dev::uid_audit::parse_hint`.
pub const HINT_FIELD_SEP: char = '\u{1f}';

/// One side (first or second) of a uid-collision pair. Mirrors the fields
/// that [`format_hint`] writes and [`parse_hint`](../../../ecp_cli/commands/dev/uid_audit/fn.parse_hint.html)
/// reads back.
#[derive(Debug, Clone, Copy)]
pub struct HintFields<'a> {
    pub kind: &'a str,
    pub path: &'a str,
    pub owner: &'a str,
    pub name: &'a str,
}

/// Build the `hint` payload for a uid-collision BlindSpot. Format:
///
/// ```text
/// {bs_kind}: first={k}{SEP}{p}{SEP}{o}{SEP}{n} second={k}{SEP}{p}{SEP}{o}{SEP}{n}
/// ```
///
/// where `{SEP}` is [`HINT_FIELD_SEP`] (U+001F). Inverse of
/// `ecp_cli::commands::dev::uid_audit::parse_hint`.
pub fn format_hint(bs_kind: &str, first: HintFields<'_>, second: HintFields<'_>) -> String {
    let sep = HINT_FIELD_SEP;
    format!(
        "{bs_kind}: first={}{sep}{}{sep}{}{sep}{} second={}{sep}{}{sep}{}{sep}{}",
        first.kind,
        first.path,
        first.owner,
        first.name,
        second.kind,
        second.path,
        second.owner,
        second.name,
    )
}

/// Per-Calls-edge dispatch metadata. Sparse: only present for `Edge` whose
/// `rel_type` is `RelType::Calls`. Sorted by `edge_idx` for binary-search
/// lookup in `graph_query.rs` hot paths.
///
/// LLM utility filter (CLAUDE.md): passes (C) Edge semantics — without
/// `is_direct`, an LLM refactor of a virtual / vtable-dispatched callee
/// would miss every dynamic callsite. The graph today is forced to either
/// emit ambiguous Calls edges or drop indirect dispatch entirely; this
/// distinguishes them at query time.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct CallMeta {
    /// Index into `ZeroCopyGraph.edges`. The edge at this index MUST have
    /// `rel_type == RelType::Calls`; population code is responsible.
    pub edge_idx: u32,
    /// Packed flags:
    /// - bit 0: `is_direct` (1 = direct call resolved statically; 0 = indirect)
    /// - bit 1: `is_dynamic_dispatch` (1 = vtable / virtual / trait-object / interface call)
    /// - bit 2: `is_callback` (1 = invoked through function-pointer / closure passed as argument)
    /// - bit 3: `is_constructor_call` (1 = invoking a constructor / `new` / Class())
    /// - bits 4-7: reserved (zero)
    pub flags: u8,
    /// When the call goes through a known dispatch type, name of that
    /// type as it appears in source (e.g. `"Box<dyn Trait>"`, `"FnPtr"`,
    /// `"foo_ops_t"` for a C struct of fn-ptrs). Empty `StrRef::NONE`-equivalent
    /// when N/A (direct call) or unknown.
    pub dispatch_type: StrRef,
}

impl CallMeta {
    pub const FLAG_DIRECT: u8 = 0b0000_0001;
    pub const FLAG_DYNAMIC_DISPATCH: u8 = 0b0000_0010;
    pub const FLAG_CALLBACK: u8 = 0b0000_0100;
    pub const FLAG_CONSTRUCTOR_CALL: u8 = 0b0000_1000;

    pub const fn is_direct(&self) -> bool {
        self.flags & Self::FLAG_DIRECT != 0
    }
    pub const fn is_dynamic_dispatch(&self) -> bool {
        self.flags & Self::FLAG_DYNAMIC_DISPATCH != 0
    }
    pub const fn is_callback(&self) -> bool {
        self.flags & Self::FLAG_CALLBACK != 0
    }
    pub const fn is_constructor_call(&self) -> bool {
        self.flags & Self::FLAG_CONSTRUCTOR_CALL != 0
    }
}

/// Per-Function/Method/Constructor-node metadata. Sparse: only present
/// for `Node` whose `kind` is `Function`, `Method`, or `Constructor`.
/// Sorted by `node_idx` for binary-search lookup.
///
/// LLM utility filter (CLAUDE.md): passes (C) Edge semantics for `is_test`
/// (excluding test callers from prod refactor queries) and (A) Graph
/// completeness for `params` / `return_type` (signature-aware resolution,
/// e.g. distinguishing overloaded methods).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct FunctionMeta {
    /// Index into `ZeroCopyGraph.nodes`. Population code MUST verify the
    /// node's kind is `Function`, `Method`, or `Constructor`.
    pub node_idx: u32,
    /// Packed flags (16-bit because we encode visibility):
    /// - bit 0: `is_test`     (test framework annotation OR file is `Test` category)
    /// - bit 1: `is_async`    (async / coroutine / suspend)
    /// - bit 2: `is_static`   (no implicit receiver — class method in OO langs)
    /// - bit 3: `is_abstract` (no body — interface / pure-virtual / abstract)
    /// - bit 4: `is_generator` (yield / function* / iterator)
    /// - bit 5: `is_extern`   (FFI declared, no Rust/native body)
    /// - bits 6-8: visibility (0=public, 1=protected, 2=private, 3=crate/internal, 4=package, 5=fileprivate, 6-7=reserved)
    /// - bits 9-15: reserved (zero)
    pub flags: u16,
    /// Parameter list, flat-encoded as alternating name/type StrRefs:
    /// `[name1, type1, name2, type2, ...]`. Empty Vec when zero params or
    /// signature was not parseable. Type StrRef may be `StrRef::NONE`-equivalent
    /// when annotation absent (dynamic-typed langs).
    pub params: Vec<StrRef>,
    /// Return type as written in source. `StrRef::NONE`-equivalent when
    /// void / unit / not annotated.
    pub return_type: StrRef,
    /// Decorator / annotation names attached to the function, in source
    /// order. E.g. `["property", "cached_property"]`, `["app.get"]`,
    /// `["@Override", "@Nullable"]`. Empty Vec when none.
    pub decorators: Vec<StrRef>,
}

impl FunctionMeta {
    pub const FLAG_TEST: u16 = 0b0000_0000_0000_0001;
    pub const FLAG_ASYNC: u16 = 0b0000_0000_0000_0010;
    pub const FLAG_STATIC: u16 = 0b0000_0000_0000_0100;
    pub const FLAG_ABSTRACT: u16 = 0b0000_0000_0000_1000;
    pub const FLAG_GENERATOR: u16 = 0b0000_0000_0001_0000;
    pub const FLAG_EXTERN: u16 = 0b0000_0000_0010_0000;

    pub const fn is_test(&self) -> bool {
        self.flags & Self::FLAG_TEST != 0
    }
    pub const fn is_async(&self) -> bool {
        self.flags & Self::FLAG_ASYNC != 0
    }
    pub const fn is_static(&self) -> bool {
        self.flags & Self::FLAG_STATIC != 0
    }
    pub const fn is_abstract(&self) -> bool {
        self.flags & Self::FLAG_ABSTRACT != 0
    }
    pub const fn is_generator(&self) -> bool {
        self.flags & Self::FLAG_GENERATOR != 0
    }
    pub const fn is_extern(&self) -> bool {
        self.flags & Self::FLAG_EXTERN != 0
    }
    /// 3-bit visibility code (0-7). 0 = public (default).
    pub const fn visibility(&self) -> u8 {
        ((self.flags >> 6) & 0b111) as u8
    }
}

pub enum Visibility {
    Public = 0,
    Protected = 1,
    Private = 2,
    Crate = 3,
    Package = 4,
    FilePrivate = 5,
}

#[derive(Archive, Deserialize, Serialize, Debug)]
#[rkyv(derive(Debug))]
pub struct ZeroCopyGraph {
    pub magic: [u8; 8],
    pub version: u32,
    pub fingerprint: [u8; 32],
    pub string_pool: Vec<u8>,
    pub files: Vec<File>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub out_offsets: Vec<u32>,
    pub in_offsets: Vec<u32>,
    pub in_edge_idx: Vec<u32>,
    /// Sorted `(xxh3_64(name), node_idx)` pairs — see `NameIndexEntry`.
    /// Replaces the v8 placeholder `Vec<u32>` that was never populated.
    pub name_index: Vec<NameIndexEntry>,

    /// Boundary index: `nodes[process_start..]` are all `NodeKind::Process`.
    /// For node_idx >= process_start, `process_k = node_idx - process_start`
    /// and its trace lives in `traces_data[traces_offsets[k]..traces_offsets[k+1]]`.
    pub process_start: u32,
    /// CSR-style boundary offsets — `traces_offsets[k+1]` is read for every
    /// process, so the vector must contain at least one element even when no
    /// processes exist. The canonical zero-process value is `vec![0]` (a
    /// single sentinel); `Default::default()` for `ZeroCopyGraph` initializes
    /// it to that. Empty `vec![]` would make `offsets[k+1]` panic for the
    /// first process append.
    pub traces_offsets: Vec<u32>,
    pub traces_data: Vec<u32>,

    /// File-level metadata: unresolvable code patterns detected during analysis.
    /// Not graph edges — just sites the LLM should flag for manual review.
    pub blind_spots: Vec<BlindSpotRecord>,

    /// Per-Route response-shape metadata extracted from handler source.
    /// Sparse: only Routes whose handler had a parseable `.json({...})` /
    /// `json_encode([...])` payload appear here. `shape_check` joins this
    /// against `RelType::Fetches` edge reasons to find consumer drift.
    pub route_shapes: Vec<RouteShape>,

    // ── Schema v5 additions ──────────────────────────────────────────
    /// Per-Calls-edge dispatch metadata. Sparse, sorted by `edge_idx`.
    /// Empty when no indirect-dispatch capture has run yet (Tasks #11/#12).
    pub call_metas: Vec<CallMeta>,
    /// Per-Function/Method/Constructor metadata. Sparse, sorted by `node_idx`.
    /// Empty when no per-language flag extraction has run yet (Task #11).
    pub function_metas: Vec<FunctionMeta>,

    // ── Schema v10 additions ─────────────────────────────────────────
    /// CSR index over `NodeKind`. Length `NodeKind::VARIANT_COUNT + 1`.
    /// For kind `k`, the node indices with that kind live at
    /// `kind_node_idx[kind_offsets[k.as_index()] .. kind_offsets[k.as_index() + 1]]`.
    /// Empty Vec is the "v9 graph upgraded in place" placeholder — the
    /// helper falls back to a linear scan when this is empty.
    pub kind_offsets: Vec<u32>,
    /// Flat node-index array sorted by kind, partitioned by `kind_offsets`.
    /// Same node may NOT appear twice (each node has exactly one kind).
    pub kind_node_idx: Vec<u32>,
}

impl ZeroCopyGraph {
    pub fn call_meta(&self, edge_idx: u32) -> Option<&CallMeta> {
        self.call_metas
            .binary_search_by_key(&edge_idx, |m| m.edge_idx)
            .ok()
            .map(|i| &self.call_metas[i])
    }

    pub fn function_meta(&self, node_idx: u32) -> Option<&FunctionMeta> {
        self.function_metas
            .binary_search_by_key(&node_idx, |m| m.node_idx)
            .ok()
            .map(|i| &self.function_metas[i])
    }
}

impl ArchivedZeroCopyGraph {
    /// Iterate node indices with the given kind via the v10 kind_offsets
    /// CSR. O(result_count). Falls back to a linear scan when kind_offsets
    /// is empty (legacy v9-shaped graph upgraded in place).
    pub fn nodes_by_kind(&self, kind: NodeKind) -> Box<dyn Iterator<Item = u32> + '_> {
        let kidx = kind.as_index();
        if self.kind_offsets.len() <= kidx + 1 {
            // No CSR — fall back to linear scan. Materialises as a Vec so the
            // returned trait object's lifetime is independent of any borrowed
            // iterator state.
            let target = kind as u8;
            return Box::new(self.nodes.iter().enumerate().filter_map(move |(i, n)| {
                // ArchivedNodeKind has the same repr(u8) layout as NodeKind.
                let nk: u8 = unsafe { *(&n.kind as *const _ as *const u8) };
                if nk == target {
                    Some(i as u32)
                } else {
                    None
                }
            }));
        }
        let start = self.kind_offsets[kidx].to_native() as usize;
        let end = self.kind_offsets[kidx + 1].to_native() as usize;
        Box::new(self.kind_node_idx[start..end].iter().map(|i| i.to_native()))
    }

    /// Look up all node indices with the given name via the v9 name_index.
    /// O(log N + hash_collisions). Returns an iterator that filters
    /// collisions by re-resolving each candidate's name from the string pool.
    ///
    /// Empty result is valid: name not present in the graph. Callers that
    /// need to know if the symbol was indexed at all should also check
    /// `name_index.is_empty()` to differentiate v8-built caches (empty
    /// placeholder) from genuine "no match" — though v9-only graphs are the
    /// only ones engine::load accepts.
    pub fn nodes_by_name<'a>(&'a self, name: &'a str) -> impl Iterator<Item = u32> + 'a {
        let target_hash = crate::uid::xxh3_64_bytes(name.as_bytes());
        let entries = self.name_index.as_slice();
        // Binary search for ANY entry with the target hash; then walk
        // outward to collect all entries sharing that hash (sorted index).
        let start = entries.partition_point(|e| e.name_hash.to_native() < target_hash);
        entries
            .iter()
            .skip(start)
            .take_while(move |e| e.name_hash.to_native() == target_hash)
            .filter_map(move |e| {
                let idx = e.node_idx.to_native();
                let node = &self.nodes[idx as usize];
                if node.name.resolve(&self.string_pool) == name {
                    Some(idx)
                } else {
                    None
                }
            })
    }
}

/// Empty-but-header-valid graph for synthetic fixtures. New schema fields
/// added to `ZeroCopyGraph` get a zero/empty default here — test fixtures
/// using `..Default::default()` absorb the addition with no churn (the
/// failure pattern that broke `heuristic_filter_structural` after the
/// schema v5 merge).
impl Default for ZeroCopyGraph {
    fn default() -> Self {
        Self {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: Vec::new(),
            files: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            out_offsets: Vec::new(),
            in_offsets: Vec::new(),
            in_edge_idx: Vec::new(),
            name_index: Vec::new(),
            process_start: 0,
            traces_offsets: vec![0],
            traces_data: Vec::new(),
            blind_spots: Vec::new(),
            route_shapes: Vec::new(),
            call_metas: Vec::new(),
            function_metas: Vec::new(),
            kind_offsets: Vec::new(),
            kind_node_idx: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::StringPool;
    use rkyv::rancor::Error;

    fn make_base_graph(pool: StringPool, name_ref: StrRef, uid_val: u64) -> ZeroCopyGraph {
        ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![File {
                path: name_ref,
                mtime: 0,
                content_hash: [0; 8],
                category: FileCategory::Source,
            }],
            nodes: vec![Node {
                uid: uid_val,
                name: name_ref,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (1, 0, 5, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            }],
            edges: vec![Edge {
                source: 0,
                target: 0,
                rel_type: RelType::Calls,
                confidence: 1.0,
                reason: name_ref,
            }],
            out_offsets: vec![0, 0],
            in_offsets: vec![0, 0],
            in_edge_idx: vec![],
            name_index: vec![],
            process_start: 1,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
            kind_offsets: vec![],
            kind_node_idx: vec![],
        }
    }

    #[test]
    fn test_serialize_deserialize_graph() {
        let mut pool = StringPool::new();
        let name_ref = pool.add("main");
        let uid_val = crate::uid::compute(NodeKind::Function, "src/main.ts", None, "main");

        let graph = make_base_graph(pool, name_ref, uid_val);

        // Serialize
        let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();

        // Deserialize / Zero-copy access
        let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(&bytes).unwrap();

        assert_eq!(archived.magic, GRAPH_MAGIC);
        assert_eq!(archived.version.to_native(), GRAPH_FORMAT_VERSION);
        assert_eq!(archived.nodes.len(), 1);

        // Resolve string using the archived string pool
        let archived_node = &archived.nodes[0];
        let name_str = archived_node.name.resolve(&archived.string_pool);
        assert_eq!(name_str, "main");
        assert_eq!(archived_node.uid.to_native(), uid_val);
    }

    #[test]
    fn test_side_table_roundtrip() {
        let mut pool = StringPool::new();
        let name_ref = pool.add("main");
        let uid_val = crate::uid::compute(NodeKind::Function, "src/main.ts", None, "main");
        let dispatch_ref = pool.add("Box<dyn Trait>");

        let mut graph = make_base_graph(pool, name_ref, uid_val);
        graph.call_metas = vec![CallMeta {
            edge_idx: 0,
            flags: CallMeta::FLAG_DYNAMIC_DISPATCH,
            dispatch_type: dispatch_ref,
        }];
        graph.function_metas = vec![FunctionMeta {
            node_idx: 0,
            flags: FunctionMeta::FLAG_ASYNC | FunctionMeta::FLAG_TEST,
            params: vec![name_ref],
            return_type: name_ref,
            decorators: vec![name_ref],
        }];

        let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();
        let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(&bytes).unwrap();

        assert_eq!(archived.call_metas.len(), 1);
        let cm = &archived.call_metas[0];
        assert_eq!(cm.edge_idx.to_native(), 0);
        assert_eq!(cm.flags, CallMeta::FLAG_DYNAMIC_DISPATCH);
        let dt = cm.dispatch_type.resolve(&archived.string_pool);
        assert_eq!(dt, "Box<dyn Trait>");

        assert_eq!(archived.function_metas.len(), 1);
        let fm = &archived.function_metas[0];
        assert_eq!(fm.node_idx.to_native(), 0);
        assert!(fm.flags.to_native() & FunctionMeta::FLAG_ASYNC != 0);
        assert!(fm.flags.to_native() & FunctionMeta::FLAG_TEST != 0);
        assert_eq!(fm.params.len(), 1);
        assert_eq!(fm.decorators.len(), 1);
    }

    #[test]
    fn test_call_meta_binary_search() {
        let mut pool = StringPool::new();
        let name_ref = pool.add("f");
        let uid_val = crate::uid::compute(NodeKind::Function, "src/f.rs", None, "f");
        let empty_ref = pool.add("");

        let mut graph = make_base_graph(pool, name_ref, uid_val);
        // 10 entries at even edge_idx values: 0, 2, 4, ..., 18
        graph.call_metas = (0u32..10)
            .map(|i| CallMeta {
                edge_idx: i * 2,
                flags: CallMeta::FLAG_DIRECT,
                dispatch_type: empty_ref,
            })
            .collect();

        assert!(graph.call_meta(4).is_some(), "edge_idx=4 must be found");
        assert!(graph.call_meta(5).is_none(), "edge_idx=5 must not be found");
        assert!(graph.call_meta(0).is_some(), "edge_idx=0 must be found");
        assert!(graph.call_meta(18).is_some(), "edge_idx=18 must be found");
        assert!(
            graph.call_meta(19).is_none(),
            "edge_idx=19 must not be found"
        );
    }

    #[test]
    fn test_struct_sizes() {
        // Sanity-check layout assumptions documented in the PR body.
        // CallMeta: edge_idx(4) + flags(1) + padding(3) + dispatch_type(8) = 16
        // FunctionMeta: node_idx(4) + flags(2) + padding(2) + params(24) + return_type(8) + decorators(24) = 64
        let call_meta_size = std::mem::size_of::<CallMeta>();
        let fn_meta_size = std::mem::size_of::<FunctionMeta>();
        println!("CallMeta size: {call_meta_size}");
        println!("FunctionMeta size: {fn_meta_size}");
        assert!(
            call_meta_size <= 24,
            "CallMeta unexpectedly large: {call_meta_size}"
        );
        assert!(
            fn_meta_size <= 72,
            "FunctionMeta unexpectedly large: {fn_meta_size}"
        );
    }
}
