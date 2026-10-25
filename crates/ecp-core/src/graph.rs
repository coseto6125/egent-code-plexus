use crate::analyzer::types::BindingKind;
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
/// v9: `name_index: Vec<NameIndexEntry>` populated for name lookup.
/// v10: `kind_offsets` / `kind_node_idx` CSR added for NodeKind lookup.
/// v11: `node_flags: Vec<u8>` added for dense per-node FunctionMeta flags.
/// v13: per-file import / export facts (`file_imports`, `imports`,
///     `file_export_aliases`, `export_aliases`, `file_flags`,
///     `exported_nodes`) and `file_path_order`, so the overlay replays the
///     import tier's re-export hop and ES-module export rule over a clean
///     module; `ImportFact::target_file` keeps the module a TS / JS
///     re-export row resolved to, tsconfig path aliases included.
/// v14: `alias_table` (the repo's path aliases, in declaration order) and
///     `supertype_table` (each type's resolved bases, in declared order, with
///     the position of the first unresolved one), so the overlay resolves an
///     aliased specifier and walks heritage like a full reindex.
/// v15: `route_middleware` (each Route's middleware, outermost group first).
/// v16: `workspace` (the C2 Cargo crate table: each member's root, entry
///     files and crate names), so the overlay resolves `other_crate::f()`.
/// v17: `lexical_parents` (each lexically scoped declaration's parent node,
///     sorted by node), so a scoped declaration never answers a global or
///     file-level name lookup and its display owner names its parent chain.
pub const GRAPH_FORMAT_VERSION: u32 = 17;

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
            "pathliteral" | "path_literal" | "path literal" => Ok(NodeKind::PathLiteral),
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
            "USESPATHLITERAL" | "USES_PATH_LITERAL" => Ok(RelType::UsesPathLiteral),
            "READSFIELD" | "READS_FIELD" => Ok(RelType::ReadsField),
            "COMPENSATEDBY" | "COMPENSATED_BY" => Ok(RelType::CompensatedBy),
            "QUERIESTABLE" | "QUERIES_TABLE" => Ok(RelType::QueriesTable),
            _ => Err(()),
        }
    }
}

impl RelType {
    /// rkyv discriminants are append-only (see `NodeKind::ALL`). Length of
    /// the `#[repr(u8)]` variant space.
    pub const VARIANT_COUNT: usize = 23;

    /// Every variant in `#[repr(u8)]` order. The schema guard test pins
    /// `ALL[i] as u8 == i` plus the variant name for all i, so a reorder /
    /// insert / rename that would silently re-label edges in existing
    /// `graph.bin` archives fails CI instead.
    pub const ALL: [RelType; Self::VARIANT_COUNT] = [
        Self::Defines,
        Self::Imports,
        Self::Calls,
        Self::Extends,
        Self::Implements,
        Self::HasMethod,
        Self::HasProperty,
        Self::Accesses,
        Self::HandlesRoute,
        Self::StepInProcess,
        Self::References,
        Self::Fetches,
        Self::MirrorsField,
        Self::Publishes,
        Self::Subscribes,
        Self::EventTopicMirror,
        Self::OpensTxScope,
        Self::Overrides,
        Self::Decorates,
        Self::UsesPathLiteral,
        Self::ReadsField,
        Self::CompensatedBy,
        Self::QueriesTable,
    ];

    pub const fn is_heuristic(self) -> bool {
        matches!(
            self,
            Self::MirrorsField | Self::EventTopicMirror | Self::CompensatedBy
        )
    }

    /// Mirror of [`ArchivedRelType::is_scope_containment`] for the owned enum.
    /// Scope-containment edges describe "where a symbol lives", not "who
    /// calls or reaches it" — impact BFS skips them. Kept in sync with the
    /// archived variant; `schema reltypes` reads this to publish the
    /// `impact_traversal` column.
    pub const fn is_scope_containment(self) -> bool {
        matches!(
            self,
            Self::Defines | Self::HasMethod | Self::HasProperty | Self::Imports
        )
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
            Self::UsesPathLiteral => "UsesPathLiteral",
            Self::ReadsField => "ReadsField",
            Self::CompensatedBy => "CompensatedBy",
            Self::QueriesTable => "QueriesTable",
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
    /// Path-shaped string literal: `"session_meta.json"`, `"crates/foo/Cargo.toml"`,
    /// `"src/lib.rs"`. Emitted from `string_literal` captures that satisfy
    /// `ecp_analyzer::path_literal::is_path_shaped` (contains `/` or `\` with
    /// non-escape continuation, or ends with a known config/data suffix).
    /// LLM-utility (B) Node coverage: without this, refactor agents fall back
    /// to grep when asking "who reads this file"; the resulting context is
    /// imprecise (matches comments, format strings, irrelevant occurrences)
    /// and loses relation traversal to the enclosing function. (C) Edge
    /// semantics: the paired `UsesPathLiteral` edge carries sink classification
    /// (`sink:read` / `sink:write` / `sink:join` / `sink:free`) so a query
    /// distinguishing readers from writers resolves without re-parsing.
    /// Cross-language: same kind serves Rust `"x.json"`, Python `'x.json'`,
    /// Go `"x.json"`, etc.; per-language predicate refinements live in
    /// `path_literal::is_path_shaped` (escape-sequence rules differ by lang).
    PathLiteral,
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

    /// True for the type kinds a call site can construct (`new A()`, `A()`,
    /// `A.new`), which then take the `Calls` edge of that construction.
    pub const fn is_constructible(self) -> bool {
        matches!(self, Self::Class | Self::Struct)
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

    /// Resolution target for `ReadsField`: a struct/class field. `SchemaField`
    /// is excluded — it is a heuristic ORM-mirror node, not a source-level
    /// field a reader expression resolves to.
    pub const fn is_property(self) -> bool {
        matches!(self, Self::Property)
    }

    /// Category super-labels for Cypher: `:Callable` / `:Type` / `:Data`
    /// expand to their member kinds at label-resolution time (case-insensitive,
    /// like `FromStr`). Exists because the Function/Method/Constructor split is
    /// load-bearing for dispatch queries but a silent-miss trap for the common
    /// agent shapes — a `:Function`-only orphan query skips every Method.
    /// Membership mirrors `is_callable` / `is_type`; `Data` covers the
    /// source-level value-holding kinds plus their DB/heuristic mirrors.
    pub fn expand_category_label(label: &str) -> Option<&'static [NodeKind]> {
        match label.to_lowercase().as_str() {
            "callable" => Some(&[Self::Function, Self::Method, Self::Constructor]),
            "type" => Some(&[
                Self::Class,
                Self::Interface,
                Self::Struct,
                Self::Enum,
                Self::Typedef,
                Self::Trait,
            ]),
            "data" => Some(&[
                Self::Property,
                Self::Variable,
                Self::Const,
                Self::EnumVariant,
                Self::SchemaField,
            ]),
            _ => None,
        }
    }

    /// Number of `NodeKind` variants. Used to size the v10 `kind_offsets`
    /// CSR array (`length = VARIANT_COUNT + 1`). Append-only schema rule
    /// means this only ever grows; matching the variant total at the bottom
    /// of the enum keeps the CI green when a new kind lands.
    pub const VARIANT_COUNT: usize = 29;

    /// Every variant in `#[repr(u8)]` order. The guard test pins
    /// `ALL[i].as_index() == i` for all i, so a new variant that is appended
    /// to the enum but not here (or vice versa) fails CI — this is what lets
    /// other code (category-label guard tests, exhaustive dumps) iterate the
    /// kind space without growing its own drift-prone list.
    pub const ALL: [NodeKind; Self::VARIANT_COUNT] = [
        Self::File,
        Self::Function,
        Self::Class,
        Self::Method,
        Self::Interface,
        Self::Constructor,
        Self::Property,
        Self::Variable,
        Self::Const,
        Self::Import,
        Self::Route,
        Self::Process,
        Self::Document,
        Self::Section,
        Self::EntryPoint,
        Self::Struct,
        Self::Enum,
        Self::Typedef,
        Self::Namespace,
        Self::Module,
        Self::Macro,
        Self::Annotation,
        Self::Trait,
        Self::Impl,
        Self::SchemaField,
        Self::EventTopic,
        Self::TransactionScope,
        Self::EnumVariant,
        Self::PathLiteral,
    ];

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
            Self::PathLiteral => "PathLiteral",
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
    /// nearest applicable declaration of its name along the declared bases
    /// of the source's type: an overload or a static is skipped by the
    /// signature in its function meta. Distinct from class-level `Extends` / `Implements`
    /// — those link `Class`/`Interface` nodes, while `Overrides` links method
    /// nodes. LLM-utility (A) Graph completeness: refactoring a base method
    /// must find every overriding implementation; without this edge the only
    /// option is grep-and-pray. (C) Edge semantics: `Extends` carries a
    /// different meaning (type hierarchy) and cannot substitute.
    ///
    /// Target is the **nearest** declaration, per the language's lookup
    /// order. For `C extends B extends A; A.foo; B.foo; C.foo` the edges
    /// are `C.foo → B.foo` and `B.foo → A.foo` (C overrides B's contract,
    /// not A's directly); when B lacks `foo`, the edge is `C.foo → A.foo`.
    /// A gap on the way (unresolved or ambiguous base, overload, cycle)
    /// gives an `override-*` BlindSpot instead of an edge past it.
    ///
    /// Languages and evidence: `ecp_analyzer::post_process::overrides`.
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
    /// Enclosing `Function` / `Method` / `File` → `PathLiteral` it embeds.
    /// `Edge.reason` carries the sink classification produced by
    /// `ecp_analyzer::path_literal::classify_sink`:
    ///   `sink:read`         — direct read (`fs::read_to_string("x")`)
    ///   `sink:write`        — direct write (`fs::write("x", …)`)
    ///   `sink:open-read`    — `File::open("x")`, `OpenOptions::open` read-only
    ///   `sink:open-write`   — `File::create("x")`, write-mode opens
    ///   `sink:join`         — `Path::new("x")`, `.join("x")`, etc.
    ///   `sink:ext-change`   — `.with_extension("x")` / `.with_file_name("x")`
    ///   `sink:free`         — bound to a `let`, returned, or passed un-classified
    /// Optionally suffixed with `|confidence:high|medium|low` when the sink
    /// match is method-name-only (e.g. `.join` is overloaded across `Path`,
    /// `Vec`, `Iterator`); resolution-quality is encoded so an LLM consumer
    /// can choose strict vs. permissive filtering without re-parsing.
    /// Appended at the END to preserve rkyv discriminants for existing
    /// `graph.bin` files.
    UsesPathLiteral,
    /// `Function` / `Method` → a struct/class field (`Property`) it reads,
    /// e.g. `obj.rel_path`, `self.count`, `cfg->timeout` (reason
    /// `field_read`), or a module-level / static constant or variable
    /// (`Const` / `Variable`, or the reader's own type's `Property`) it
    /// names bare, e.g. `LIMITS` in `readLimit()` (reason `value_read`).
    ///
    /// LLM-utility filter (A) Graph completeness: changing the meaning or type
    /// of a field (rename, repr change, semantic shift) has a blast radius
    /// spanning every reader across files. Without this edge `ecp impact
    /// <field>` returns empty — indistinguishable from "no impact" — so an LLM
    /// refactor silently misses readers. With it, the readers are one CSR hop
    /// away, the same guarantee `Calls` gives for functions.
    ///
    /// Does NOT contradict the panel decision to drop function-body locals
    /// (see `drop_locals`): a `field_read` resolves only to `Property`
    /// targets (the `ResolveTarget::Field` / `is_property` filter), and a
    /// `value_read` only to declarations the graph already holds, which no
    /// function-body local is; the parser drops a bare name that a
    /// parameter or local of the reader declares. So the local fan-out the
    /// panel rejected can never appear here. Both public and private fields
    /// emit edges — a private field's readers are just as load-bearing for an
    /// in-module refactor as a public one's, and omitting them would put the
    /// "empty == no impact" ambiguity right back.
    ///
    /// Appended at the END to preserve rkyv discriminants for existing
    /// `graph.bin` files.
    ReadsField,
    /// Heuristic Saga compensation edge: `compensator → operation`. Source is a
    /// `compensate_/undo_/rollback_<verb_noun>` callable; target is the same-class
    /// `<verb_noun>` operation it rolls back. `Edge.reason` encodes evidence tier:
    /// `saga:calls-back` (confidence 0.8, compensator has a `Calls` edge to the
    /// operation) or `saga:name-only` (0.6, name-pair only — real Sagas often
    /// trigger compensation via an orchestrator, so name-only pairs are still
    /// emitted). Low-confidence — `is_heuristic()` returns `true`, so default
    /// `ecp impact` hides it.
    ///
    /// LLM-utility (A) Graph completeness: refactoring a Saga operation, `ecp
    /// impact <operation>` BFS surfaces its compensator directly — no grepping
    /// `undo_*`/`rollback_*` name conventions. (C) Edge semantics: `Calls` only
    /// says "A calls B"; it cannot express "B is A's failure compensation" — the
    /// directional rollback semantic that this edge carries.
    /// Appended at the END to preserve rkyv discriminants for existing
    /// `graph.bin` files.
    CompensatedBy,
    /// `Function` / `Method` → a database `Class` (table) it reads or writes
    /// via a raw SQL string literal. `Edge.reason` carries `"read"` (SELECT) or
    /// `"write"` (INSERT/UPDATE/DELETE). LLM-utility (A) Graph completeness:
    /// without this edge `ecp impact <table> --upstream` omits every caller
    /// that touches the table through raw SQL, so a schema-migration query
    /// silently misses breaking callers. Appended at the END for rkyv
    /// discriminant stability.
    QueriesTable,
}

impl ArchivedRelType {
    /// Mirror of `RelType::is_heuristic` for zero-copy graph traversal.
    pub const fn is_heuristic(&self) -> bool {
        matches!(
            self,
            Self::MirrorsField | Self::EventTopicMirror | Self::CompensatedBy
        )
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
    /// Index into `ZeroCopyGraph.files`. The sentinel value
    /// [`SYNTHETIC_FILE_IDX`] (`u32::MAX`) marks a node with no single
    /// owning file — currently emitted by `post_process::decorates_edges`
    /// for resolver-miss fallback `Annotation` nodes shared across
    /// multiple call sites. Consumers that index `graph.files[file_idx]`
    /// MUST guard against this sentinel before dereferencing; see
    /// [`Node::has_owning_file`].
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

/// Sentinel `Node.file_idx` value marking a synthetic node with no single
/// owning file. Used by `post_process::decorates_edges` for resolver-miss
/// fallback `Annotation` nodes that may be referenced by `Decorates` edges
/// from multiple files. Consumers iterating `graph.nodes` and indexing
/// `graph.files[node.file_idx]` MUST guard against this value — otherwise
/// the index `u32::MAX as usize` triggers a slice OOB panic. Use
/// [`Node::has_owning_file`] / [`ArchivedNode::has_owning_file`] to skip.
pub const SYNTHETIC_FILE_IDX: u32 = u32::MAX;

impl Node {
    /// True iff this node owns a real file entry in `ZeroCopyGraph.files`.
    /// False for synthetic placeholders (currently: resolver-miss
    /// fallback `Annotation` nodes from `Decorates` edges). Use as a guard
    /// before `graph.files[node.file_idx as usize]`.
    #[inline]
    pub fn has_owning_file(&self) -> bool {
        self.file_idx != SYNTHETIC_FILE_IDX
    }
}

impl ArchivedNode {
    /// Zero-copy mirror of [`Node::has_owning_file`].
    #[inline]
    pub fn has_owning_file(&self) -> bool {
        self.file_idx.to_native() != SYNTHETIC_FILE_IDX
    }

    /// 1-based start line for display. `span.0` is the tree-sitter row
    /// (0-based), kept raw internally for range/hash/containment; every
    /// user-facing `line` is +1 so it matches editors, `grep -n`, compiler
    /// diagnostics, and the LLM's 1-based prior — the single conversion
    /// boundary between tree-sitter's convention and the consumer's.
    #[inline]
    pub fn start_line(&self) -> u32 {
        self.span.0.to_native().saturating_add(1)
    }

    /// 1-based end line (`span.2` + 1). Same convention as [`start_line`].
    #[inline]
    pub fn end_line(&self) -> u32 {
        self.span.2.to_native().saturating_add(1)
    }
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

/// One import of a file, in source order: its
/// [`crate::analyzer::types::RawImport`], kept so the overlay can follow a
/// clean module's re-export without the module's parse.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct ImportFact {
    pub source: StrRef,
    pub imported_name: StrRef,
    pub alias: Option<StrRef>,
    pub binding_kind: Option<BindingKind>,
    /// The index of the file the index resolved `source` to, for a TS / JS
    /// row the re-export hop follows: `graph.bin` keeps no tsconfig path
    /// aliases, so the overlay cannot resolve an aliased specifier itself.
    /// `None` for every other row and for a specifier naming no indexed file.
    pub target_file: Option<u32>,
}

/// One `(exported, local)` pair of a TS / JS file's
/// [`crate::analyzer::types::RawExportAlias`] list.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct ExportAliasFact {
    pub exported: StrRef,
    pub local: StrRef,
}

/// One entry of the repo's path-alias table
/// ([`crate::analyzer::path_aliases::PathAliases`]): a pattern and its
/// replacement candidates. The table keeps declaration order, because the
/// resolver tries entries in that order; a repeated pattern stays repeated.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct PathAliasFact {
    pub pattern: StrRef,
    pub replacements: Vec<StrRef>,
}

/// The declared supertypes of one type node, as the index's heritage
/// pre-pass resolved them. `bases` and `first_unresolved` carry the same
/// data as the analyzer's `Supertypes`: node indices in declared order,
/// deduplicated, never the type itself, and the number of resolved bases
/// declared before the first unresolved one (`None` when all resolved).
/// Extends / Implements edges lose that position, so the overlay cannot
/// rebuild it from edges.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct SupertypeFact {
    pub type_idx: u32,
    pub bases: Vec<u32>,
    pub first_unresolved: Option<u32>,
}

/// `ZeroCopyGraph.file_flags` bit: the file is an ES module, so another file
/// imports only its exports ([`crate::analyzer::reexport::lookup_export_in_file`]).
pub const FILE_FLAG_ES_MODULE: u8 = 1;

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

/// The middleware a Route passes through, outermost group first, then the
/// route's own (Laravel `->middleware('auth')`, group `middleware` option),
/// then what the bound controller registers for the action's method
/// (`$this->middleware(...)` with `only` / `except`, static
/// `HasMiddleware::middleware()`). Each name appears once, as written, and
/// none that a `withoutMiddleware` / `excluded_middleware` removes. Not
/// included: the Kernel's global `$middleware`, and the members of a
/// middleware group (`api` stays `api`). `node_idx` points into
/// `ZeroCopyGraph.nodes`. Answers "which routes are reachable without
/// `auth`" without re-parsing route files.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RouteMiddleware {
    pub node_idx: u32,
    pub middleware: Vec<StrRef>,
}

/// One scoped node (contract C1: a declaration whose lexical parent is a
/// callable) and that parent, both indices into `ZeroCopyGraph.nodes`. A
/// scoped node has no owner, so without this row a reader cannot tell a
/// nested `helper` from a top-level one: the overlay's name tiers would
/// bind it from another scope (invariant I5), and a display owner, rename
/// or diff identity would print it as top level.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct LexicalParentFact {
    pub node_idx: u32,
    pub parent_idx: u32,
}

/// File-level record of a truly unresolvable code pattern (eval/dynamic
/// import/cross-object reflection/...). Persisted in the graph so that
/// `ecp context` / `ecp admin index` can surface blind spots to the LLM,
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
    /// True iff the BlindSpot was emitted from a file classified as test
    /// scaffolding by `is_test_path(file_path)`. The verdict layer
    /// (`INDIRECT_DISPATCH_IN_DIFF_REGION`, `BLINDSPOT_IN_DIFF_REGION`)
    /// filters these out so prod-refactor warnings don't surface eval/reflection
    /// noise from test fixtures and mocks.
    ///
    /// **Append-only field**: rkyv discriminants are positional. Placed
    /// at the end of the struct so existing `graph.bin` files remain
    /// decodable as long as readers tolerate the new trailing field
    /// (they don't — see `auto_ensure`: reindex triggers automatically on
    /// schema diff). New parsers SHOULD populate this; older `graph.bin`
    /// before this field landed will need a one-time reindex.
    pub is_test: bool,
}

/// BlindSpot kind emitted by `resolution::builder::classify_collision` for
/// uid hash collisions. Exposed as a const so consumers
/// (`ecp dev uid-audit`) match against it without re-typing the literal.
pub const BS_KIND_UID_COLLISION: &str = "uid-collision";

/// Type-2 "parser-metric" BlindSpot kinds — emitted by the resolver's
/// `classify_collision` for uid hash collisions and `#ifdef`
/// redefinitions. These are parser-maintainer signal, NOT
/// LLM-actionable source-code opacity (which is Type-1: `python-eval`,
/// `python-exec`, `dynamic-import`, etc.).
///
/// `ecp summary.blind_spots` filters these out so its LLM-facing surface
/// stays focused; `ecp dev uid-audit` inspects them.
pub const DEV_METRIC_BS_KINDS: &[&str] = &[BS_KIND_UID_COLLISION, "ifdef-redef"];

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

/// Human / agent rendering of a stored hint. The stored form stays
/// separator-joined because `uid_audit::parse_hint` reads it back; this view
/// is built from the parsed fields. `line` is the BlindSpot's own row, which
/// is the colliding (second) declaration; the first declaration's line is not
/// recorded. A hint that is not a collision pair is returned as it is, and a
/// malformed one only loses its separators.
pub fn render_hint(hint: &str, line: u32) -> String {
    let Some((bs_kind, rest)) = hint.split_once(": first=") else {
        return hint.to_string();
    };
    let Some((first, second)) = rest.rsplit_once(" second=") else {
        return hint.replace(HINT_FIELD_SEP, " ");
    };
    fn parse(side: &str) -> Option<HintFields<'_>> {
        let mut it = side.splitn(4, HINT_FIELD_SEP);
        Some(HintFields {
            kind: it.next()?,
            path: it.next()?,
            owner: it.next()?,
            name: it.next()?,
        })
    }
    let (Some(a), Some(b)) = (parse(first), parse(second)) else {
        return hint.replace(HINT_FIELD_SEP, " ");
    };
    let qualified = |f: &HintFields<'_>| {
        if f.owner.is_empty() {
            f.name.to_string()
        } else {
            format!("{}.{}", f.owner, f.name)
        }
    };
    format!(
        "{bs_kind}: first={} {} {} second={} {}:{line} {}",
        a.kind,
        a.path,
        qualified(&a),
        b.kind,
        b.path,
        qualified(&b),
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
    /// - bit 3: reserved, formerly `is_constructor_call`. No extractor sets
    ///   it; graphs written before its writer was removed may still hold it.
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
    /// Reserved: no extractor writes this bit. It stays defined because
    /// stored graphs may carry it.
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
    /// - bit 9: `is_virtual`  (C++ only: declared `virtual`, or an `override` / `final` member)
    /// - bit 10: C# extension method (static method of a static class whose first parameter carries `this`)
    /// - bit 11: C# extension on a generic type parameter (`this T`)
    /// - bit 12: C# extension method whose parameter list ends in a `params` array (absent from `params`)
    /// - bits 13-15: C# extension method's count of default-valued parameters, saturating at 7
    ///
    /// Bits 12-15 stay zero on every method that is no C# extension method.
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
    /// Bit 9; bits 6-8 hold the visibility code. The `Overrides` pass reads
    /// it: a C++ base method overrides only when it is virtual.
    pub const FLAG_VIRTUAL: u16 = 0b0000_0010_0000_0000;

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

    /// Index of the first `NodeKind::Process` node. The Process block starts
    /// here and spans `traces_offsets.len() - 1` nodes; do NOT assume it runs to
    /// `nodes.len()` — later passes (PathLiteral promotion, File append) push
    /// non-Process nodes after it. For the k-th process (node_idx =
    /// process_start + k), its trace is `traces_data[traces_offsets[k]..traces_offsets[k+1]]`.
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

    // ── Schema v11 additions ─────────────────────────────────────────
    /// Dense low-byte FunctionMeta flags by node index. Length matches
    /// `nodes.len()` for v11 graphs. Non-function nodes and function nodes
    /// without FunctionMeta use 0. This keeps hot boolean flag filters out of
    /// the sparse `function_metas` binary-search side table.
    pub node_flags: Vec<u8>,

    // ── Schema v13 additions ─────────────────────────────────────────
    /// CSR offsets over `files`, length `files.len() + 1`: the imports of
    /// file `f` are `imports[file_imports[f] .. file_imports[f + 1]]`.
    /// Recorded for all parsed languages; a file without imports has an empty
    /// range.
    pub file_imports: Vec<u32>,
    pub imports: Vec<ImportFact>,
    /// CSR offsets over `files` into `export_aliases`, like `file_imports`:
    /// a TS / JS file's `LocalGraph.export_aliases` pairs.
    pub file_export_aliases: Vec<u32>,
    pub export_aliases: Vec<ExportAliasFact>,
    /// One byte per file, see [`FILE_FLAG_ES_MODULE`].
    pub file_flags: Vec<u8>,
    /// Bit `i % 8` of byte `i / 8` is set when `nodes[i]` is an exported
    /// declaration of an ES module.
    pub exported_nodes: Vec<u8>,
    /// Indices of `files` in `Path` order, for a path lookup without a scan.
    /// Empty when `files` already is in that order, as every graph the
    /// builder makes is.
    pub file_path_order: Vec<u32>,

    // ── Schema v14 additions ─────────────────────────────────────────
    /// The path aliases the index resolved with, in declaration order.
    /// Empty when the repo has none.
    pub alias_table: Vec<PathAliasFact>,
    /// Sparse, sorted by `type_idx`: one entry per type that declares
    /// heritage. A type with no heritage has no entry.
    pub supertype_table: Vec<SupertypeFact>,

    // ── Schema v15 additions ─────────────────────────────────────────
    /// Sparse, sorted by `node_idx`: one entry per Route with at least one
    /// middleware.
    pub route_middleware: Vec<RouteMiddleware>,

    // ── Schema v16 additions ─────────────────────────────────────────
    /// Contract C2's workspace facts: one Cargo scope per workspace member
    /// and the crate names that reach it, from the manifest parse the
    /// index's module tree ran. The overlay reads it to resolve a
    /// `<crate_name>::path::f()` head like the index's module-tree tier.
    /// Empty for a repo with no root `Cargo.toml`.
    pub workspace: crate::contracts::workspace::WorkspaceFacts,

    // ── Schema v17 additions ─────────────────────────────────────────
    /// Sparse, sorted by `node_idx`: one row per scoped node, see
    /// [`LexicalParentFact`]. Empty while no language opts in
    /// ([`crate::analyzer::lexical::lexical_scope_kinds`]).
    pub lexical_parents: Vec<LexicalParentFact>,
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

    /// Indices of every node named exactly `name`, ascending: the order a
    /// `0..nodes.len()` scan visits them. `nodes_by_name` cannot promise it,
    /// because `name_index` is sorted by hash alone with an unstable sort, so
    /// same-name nodes come out in unspecified order, and callers break ties
    /// by input order.
    ///
    /// `None` means "keep your scan": an empty `name` (the index skips
    /// tombstones, which the scan still matches) or an empty `name_index`
    /// (hand-built fixtures). `Some(vec![])` is a genuine miss.
    pub fn nodes_named_sorted(&self, name: &str) -> Option<Vec<u32>> {
        if name.is_empty() || self.name_index.is_empty() {
            return None;
        }
        let mut hits: Vec<u32> = self.nodes_by_name(name).collect();
        hits.sort_unstable();
        Some(hits)
    }

    /// Whether the v13 import / export facts are present and consistent
    /// with `files` and `nodes`. An archive without them cannot tell a file
    /// with no imports from a file whose imports were never recorded, so
    /// the loader rejects it and the graph is rebuilt.
    ///
    /// O(1): the loader runs it on every load. The per-file accessors
    /// bounds-check each range they read.
    pub fn has_module_facts(&self) -> bool {
        let files = self.files.len();
        csr_consistent(&self.file_imports, files, self.imports.len())
            && csr_consistent(&self.file_export_aliases, files, self.export_aliases.len())
            && self.file_flags.len() == files
            && self.exported_nodes.len() == self.nodes.len().div_ceil(8)
            && (self.file_path_order.is_empty() || self.file_path_order.len() == files)
    }

    /// Index of the file whose path is exactly `path`: O(log files), through
    /// `file_path_order` when `files` is not in `Path` order.
    pub fn file_idx_of(&self, path: &str) -> Option<u32> {
        let pool = &self.string_pool;
        let key = std::path::Path::new(path);
        let path_of = |idx: usize| std::path::Path::new(self.files[idx].path.resolve(pool));
        let found = if self.file_path_order.is_empty() {
            self.files
                .binary_search_by(|file| std::path::Path::new(file.path.resolve(pool)).cmp(key))
                .ok()?
        } else {
            let at = self
                .file_path_order
                .binary_search_by(|idx| path_of(idx.to_native() as usize).cmp(key))
                .ok()?;
            self.file_path_order[at].to_native() as usize
        };
        // `Path` order ignores a doubled or trailing `/`.
        (self.files[found].path.resolve(pool) == path).then_some(found as u32)
    }

    /// The recorded imports of `files[file_idx]`; `None` without module
    /// facts.
    pub fn file_imports(&self, file_idx: u32) -> Option<&[ArchivedImportFact]> {
        let i = file_idx as usize;
        let start = self.file_imports.get(i)?.to_native() as usize;
        let end = self.file_imports.get(i + 1)?.to_native() as usize;
        self.imports.get(start..end)
    }

    /// The export alias pairs of `files[file_idx]`; `None` without module
    /// facts.
    pub fn file_export_aliases(&self, file_idx: u32) -> Option<&[ArchivedExportAliasFact]> {
        let i = file_idx as usize;
        let start = self.file_export_aliases.get(i)?.to_native() as usize;
        let end = self.file_export_aliases.get(i + 1)?.to_native() as usize;
        self.export_aliases.get(start..end)
    }

    /// The path aliases the index resolved with, rebuilt in declaration
    /// order; empty when the repo has none.
    pub fn path_aliases(&self) -> crate::analyzer::path_aliases::PathAliases {
        let pool = &self.string_pool;
        let mut aliases = crate::analyzer::path_aliases::PathAliases::new();
        for fact in self.alias_table.iter() {
            aliases.add(
                fact.pattern.resolve(pool),
                fact.replacements
                    .iter()
                    .map(|replacement| replacement.resolve(pool).to_string())
                    .collect(),
            );
        }
        aliases
    }

    /// `(bases, first_unresolved)` of the type node `type_idx`: the bases in
    /// declared order as node indices, and the number of resolved bases
    /// declared before the first unresolved one (`Some(0)` when the first
    /// declared base did not resolve; `None` when every base resolved).
    /// `None` when the type has no entry, that is, declares no heritage.
    pub fn supertypes_of(&self, type_idx: u32) -> Option<(Vec<u32>, Option<u32>)> {
        let at = self
            .supertype_table
            .binary_search_by_key(&type_idx, |fact| fact.type_idx.to_native())
            .ok()?;
        let fact = &self.supertype_table[at];
        Some((
            fact.bases.iter().map(|base| base.to_native()).collect(),
            fact.first_unresolved.as_ref().map(|n| n.to_native()),
        ))
    }

    /// The middleware of the Route node `node_idx`, outermost first; empty
    /// when it has none.
    pub fn route_middleware_of(&self, node_idx: u32) -> Vec<&str> {
        self.route_middleware
            .binary_search_by_key(&node_idx, |entry| entry.node_idx.to_native())
            .map(|at| {
                self.route_middleware[at]
                    .middleware
                    .iter()
                    .map(|name| name.resolve(&self.string_pool))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The stored workspace facts, owned. A table that fails
    /// [`WorkspaceFacts::validate`](crate::contracts::workspace::WorkspaceFacts::validate)
    /// reads as empty: a reader must not resolve through rows that break
    /// the contract's invariants.
    pub fn workspace_facts(&self) -> crate::contracts::workspace::WorkspaceFacts {
        rkyv::deserialize::<crate::contracts::workspace::WorkspaceFacts, rkyv::rancor::Error>(
            &self.workspace,
        )
        .ok()
        .filter(|facts| facts.validate().is_ok())
        .unwrap_or_default()
    }

    /// The lexical parent of `node_idx`: the callable that declares it.
    /// `None` for an unscoped node, which is every node while no language
    /// opts in, and for an index past `nodes`.
    pub fn lexical_parent(&self, node_idx: u32) -> Option<u32> {
        self.lexical_parents
            .binary_search_by_key(&node_idx, |fact| fact.node_idx.to_native())
            .ok()
            .map(|at| self.lexical_parents[at].parent_idx.to_native())
    }

    /// Whether `node_idx` is scoped: only the lexical tier may bind it
    /// (invariant I5), so every name-tier candidate source drops it.
    pub fn is_lexically_scoped(&self, node_idx: u32) -> bool {
        self.lexical_parent(node_idx).is_some()
    }

    /// The owner text a reader prints for `node_idx` (design A §4.5): its
    /// `owner_class` when set; for a scoped node, the display owner of its
    /// lexical parent and then the parent's name, joined by `::` (the
    /// parent's name alone when the parent has none); else `None`.
    ///
    /// A nested `helper` in `impl A { fn new }` prints `A::new`. `None`
    /// also for an index past `nodes` and for rows that form a cycle, which
    /// only a corrupt table holds.
    pub fn display_owner(&self, node_idx: u32) -> Option<std::borrow::Cow<'_, str>> {
        let pool = &self.string_pool;
        let mut segments: Vec<&str> = Vec::new();
        let mut current = node_idx;
        let head = loop {
            let owner = self.nodes.get(current as usize)?.owner_class.resolve(pool);
            if !owner.is_empty() {
                break Some(owner);
            }
            let Some(parent) = self.lexical_parent(current) else {
                break None;
            };
            // Each row is one step, so more steps than rows is a cycle.
            if segments.len() >= self.lexical_parents.len() {
                return None;
            }
            segments.push(self.nodes.get(parent as usize)?.name.resolve(pool));
            current = parent;
        };
        segments.extend(head);
        segments.reverse();
        match segments.as_slice() {
            [] => None,
            [only] => Some(std::borrow::Cow::Borrowed(*only)),
            _ => Some(std::borrow::Cow::Owned(segments.join("::"))),
        }
    }

    pub fn is_es_module_file(&self, file_idx: u32) -> bool {
        self.file_flags
            .get(file_idx as usize)
            .is_some_and(|flags| *flags & FILE_FLAG_ES_MODULE != 0)
    }

    /// Whether `node_idx` is a later overload ([`crate::uid::keeps_overloads`]):
    /// a node the index keeps but never binds by name, so a name lookup that
    /// mirrors the index's resolver must skip it. Its uid is the only mark.
    pub fn is_later_overload(&self, node_idx: u32) -> bool {
        let pool = &self.string_pool;
        let node = &self.nodes[node_idx as usize];
        let kind = NodeKind::from(&node.kind);
        crate::uid::keeps_overloads(kind)
            && self
                .files
                .get(node.file_idx.to_native() as usize)
                .is_some_and(|file| {
                    node.uid.to_native()
                        != crate::uid::compute(
                            kind,
                            file.path.resolve(pool),
                            Some(node.owner_class.resolve(pool)),
                            node.name.resolve(pool),
                        )
                })
    }

    pub fn is_exported_node(&self, node_idx: u32) -> bool {
        self.exported_nodes
            .get(node_idx as usize / 8)
            .is_some_and(|bits| *bits & (1u8 << (node_idx % 8)) != 0)
    }

    /// Name-index hits in node order, or every node when the index cannot
    /// answer (empty name, empty index). Callers still compare the name.
    pub fn name_candidates(&self, name: &str) -> impl Iterator<Item = u32> + '_ {
        let (hits, all) = match self.nodes_named_sorted(name) {
            Some(h) => (h, 0..0),
            None => (Vec::new(), 0..self.nodes.len() as u32),
        };
        hits.into_iter().chain(all)
    }
}

/// Whether `offsets` can be a CSR offset table over `len` entries into
/// `rows` rows: `len + 1` offsets from 0 to `rows`.
fn csr_consistent(offsets: &[rkyv::Archived<u32>], len: usize, rows: usize) -> bool {
    offsets.len() == len + 1
        && offsets.first().map(|o| o.to_native()) == Some(0)
        && offsets.last().map(|o| o.to_native() as usize) == Some(rows)
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
            node_flags: Vec::new(),
            // The CSR sentinels of a graph with no files.
            file_imports: vec![0],
            imports: Vec::new(),
            file_export_aliases: vec![0],
            export_aliases: Vec::new(),
            file_flags: Vec::new(),
            exported_nodes: Vec::new(),
            file_path_order: Vec::new(),
            alias_table: Vec::new(),
            supertype_table: Vec::new(),
            route_middleware: Vec::new(),
            workspace: Default::default(),
            lexical_parents: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_fixture::GraphFixture;
    use rkyv::rancor::Error;

    #[test]
    fn node_kind_all_is_exhaustive_and_ordered() {
        for (i, k) in NodeKind::ALL.iter().enumerate() {
            assert_eq!(k.as_index(), i, "ALL out of repr order at {i}: {k:?}");
        }
    }

    #[test]
    fn category_labels_mirror_kind_predicates() {
        // expand_category_label's slices are the queryable face of the
        // is_callable / is_type predicates — any divergence is a schema bug.
        let callable = NodeKind::expand_category_label("callable").unwrap();
        let type_ = NodeKind::expand_category_label("type").unwrap();
        for k in NodeKind::ALL {
            assert_eq!(
                k.is_callable(),
                callable.contains(&k),
                ":Callable diverges from is_callable for {k:?}"
            );
            assert_eq!(
                k.is_type(),
                type_.contains(&k),
                ":Type diverges from is_type for {k:?}"
            );
        }
    }

    /// A single self-referential node: `name` (at `path`) calls itself.
    /// Returns the still-open fixture plus the node's index and the
    /// self-loop edge's (pre-sort) index, so a test can layer call/function
    /// meta on top before calling `build`.
    fn make_base_fixture(path: &str, name: &str) -> (GraphFixture, u32, u32) {
        let mut fx = GraphFixture::new();
        let node = fx.func(path, name);
        fx.span(node, (1, 0, 5, 0));
        let edge = fx.edge_with(node, node, RelType::Calls, 1.0, name);
        (fx, node, edge)
    }

    #[test]
    fn test_serialize_deserialize_graph() {
        let (fx, node, _edge) = make_base_fixture("src/main.ts", "main");
        let graph = fx.build();

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
        assert_eq!(
            archived_node.uid.to_native(),
            graph.nodes[node as usize].uid
        );
    }

    #[test]
    fn test_side_table_roundtrip() {
        let (mut fx, node, edge) = make_base_fixture("src/main.ts", "main");
        let name_ref = fx.intern("main");
        fx.call_meta(edge, CallMeta::FLAG_DYNAMIC_DISPATCH, "Box<dyn Trait>");
        // params / return_type have no helper on GraphFixture; the escape
        // hatch is the assembly directly.
        fx.assembly_mut().function_metas.push(FunctionMeta {
            node_idx: node,
            flags: FunctionMeta::FLAG_ASYNC | FunctionMeta::FLAG_TEST,
            params: vec![name_ref],
            return_type: name_ref,
            decorators: vec![name_ref],
        });

        let graph = fx.build();
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

    fn archive(graph: &ZeroCopyGraph) -> rkyv::util::AlignedVec {
        rkyv::to_bytes::<Error>(graph).unwrap()
    }

    #[test]
    fn test_path_aliases_roundtrip_keeps_order_duplicates_and_star_free_patterns() {
        let (mut fx, ..) = make_base_fixture("src/main.ts", "main");
        let facts: Vec<PathAliasFact> = [
            ("@/*", vec!["src/*"]),
            ("config", vec!["app/config.ts"]),
            ("@/*", vec!["lib/*", "vendor/*"]),
        ]
        .into_iter()
        .map(|(pattern, replacements)| PathAliasFact {
            pattern: fx.intern(pattern),
            replacements: replacements.into_iter().map(|r| fx.intern(r)).collect(),
        })
        .collect();
        fx.assembly_mut().alias_table = facts;
        let bytes = archive(&fx.build());
        let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(&bytes).unwrap();

        let aliases = archived.path_aliases();
        let got: Vec<(String, Vec<String>)> = aliases
            .entries()
            .map(|(pattern, replacements)| (pattern.to_string(), replacements.to_vec()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("@/*".to_string(), vec!["src/*".to_string()]),
                ("config".to_string(), vec!["app/config.ts".to_string()]),
                (
                    "@/*".to_string(),
                    vec!["lib/*".to_string(), "vendor/*".to_string()]
                ),
            ]
        );
        let mut expanded = Vec::new();
        aliases.expand("@/x", |candidate| {
            expanded.push(candidate.to_string());
            true
        });
        assert_eq!(expanded, ["src/x", "lib/x", "vendor/x"]);
    }

    #[test]
    fn test_path_aliases_empty_table_returns_empty_aliases() {
        let (fx, ..) = make_base_fixture("src/main.ts", "main");
        let bytes = archive(&fx.build());
        let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(&bytes).unwrap();
        assert!(archived.path_aliases().is_empty());
    }

    #[test]
    fn test_supertypes_of_roundtrip_returns_declared_order_and_first_unresolved() {
        let (mut fx, ..) = make_base_fixture("src/main.ts", "main");
        // Declared out of order: `finish` sorts by type_idx.
        fx.assembly_mut().supertype_table = vec![
            SupertypeFact {
                type_idx: 9,
                bases: vec![],
                first_unresolved: Some(0),
            },
            SupertypeFact {
                type_idx: 4,
                bases: vec![7, 2, 5],
                first_unresolved: None,
            },
            SupertypeFact {
                type_idx: 6,
                bases: vec![3],
                first_unresolved: Some(1),
            },
        ];
        let bytes = archive(&fx.build());
        let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(&bytes).unwrap();

        assert_eq!(archived.supertypes_of(4), Some((vec![7, 2, 5], None)));
        assert_eq!(archived.supertypes_of(6), Some((vec![3], Some(1))));
        // Every declared base failed to resolve: no bases, unresolved at position 0.
        assert_eq!(archived.supertypes_of(9), Some((vec![], Some(0))));
        // A type with no heritage has no entry.
        assert_eq!(archived.supertypes_of(0), None);
        assert_eq!(archived.supertypes_of(u32::MAX), None);
    }

    #[test]
    fn test_supertypes_of_empty_table_returns_none() {
        let (fx, ..) = make_base_fixture("src/main.ts", "main");
        let bytes = archive(&fx.build());
        let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(&bytes).unwrap();
        assert_eq!(archived.supertypes_of(0), None);
    }

    #[test]
    fn test_call_meta_binary_search() {
        let (fx, ..) = make_base_fixture("src/f.rs", "f");
        let mut graph = fx.build();
        // 10 entries at even edge_idx values: 0, 2, 4, ..., 18 — deliberately
        // out of range of the fixture's single real edge; this test exercises
        // `call_meta`'s binary search in isolation, not edge-sort remapping.
        graph.call_metas = (0u32..10)
            .map(|i| CallMeta {
                edge_idx: i * 2,
                flags: CallMeta::FLAG_DIRECT,
                dispatch_type: StrRef::default(),
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
    fn queries_table_reltype_roundtrips() {
        use rkyv::rancor::Error;
        let rt = RelType::QueriesTable;
        let bytes = rkyv::to_bytes::<Error>(&rt).expect("serialize");
        let archived = rkyv::access::<ArchivedRelType, Error>(&bytes).expect("access");
        let back: RelType = RelType::from(archived);
        assert_eq!(back, RelType::QueriesTable);
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
