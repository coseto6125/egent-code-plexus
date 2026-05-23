use crate::graph::NodeKind;
use rkyv::{Archive, Deserialize, Serialize};
use std::path::PathBuf;

/// Language-agnostic function metadata captured during parsing, before the
/// string pool is available. Stored in `LocalGraph` and converted to
/// `FunctionMeta` (with interned `StrRef`s) by `GraphBuilder::build`.
///
/// Keyed by `span` — must match the `RawNode.span` of the function/method/
/// constructor it describes. The builder pairs them via span lookup.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawFunctionMeta {
    /// Span of the corresponding `RawNode` (used as the pairing key).
    pub span: (u32, u32, u32, u32),
    /// Bit-packed flags using the same layout as `FunctionMeta::flags`.
    /// Callers use `FunctionMeta::FLAG_*` constants and the 3-bit visibility
    /// shift (bits 6-8) to build this value.
    pub flags: u16,
    /// Flat alternating `[name1, type1, name2, type2, ...]` — empty String
    /// for absent type annotations (dynamic-typed languages).
    pub params: Vec<String>,
    /// Return type as written in source. Empty string when absent/void.
    pub return_type: String,
    /// Decorator/attribute names in source order (e.g. `"staticmethod"`,
    /// `"#[test]"`, `"@Injectable"`).
    pub decorators: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct IdentifierRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub row: usize,
    pub col: usize,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawNode {
    pub name: String,
    pub kind: NodeKind,
    pub span: (u32, u32, u32, u32),
    pub is_exported: bool,
    pub heritage: Vec<String>, // Base classes, interfaces, traits
    pub type_annotation: Option<String>,
    pub decorators: Vec<String>,
    /// Names of functions/methods invoked from inside this node's body.
    /// Each entry is the callee's *short* name (e.g. `method` for `obj.method()`).
    /// Resolved against imports + same-file symbols in Pass 2 → `RelType::Calls`.
    pub calls: Vec<String>,
    /// Owning class/struct/trait for methods and properties.
    /// Set by each language parser at parse time; `None` for module-level
    /// functions. Eliminates the need for post-process span containment to
    /// establish class membership — parsers have grammar-level access to the
    /// enclosing type that the post-pass must re-derive from spans.
    pub owner_class: Option<String>,
    /// xxh3-64 hash of the symbol's raw source bytes
    /// (`source[start_byte..end_byte]` from the tree-sitter node).
    /// Computed by each language parser at parse time. `0` for synthetic
    /// nodes that have no corresponding source span (e.g. delegate stubs).
    /// Used by T7-4/5/6 incremental indexers to detect unchanged symbols
    /// without re-parsing.
    pub content_hash: u64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum BindingKind {
    /// Symbol → symbol (typedef, identifier-bodied `#define`, extern decls).
    Alias,
    /// Symbol → literal value (`#define MAX 4096`, `#define VER "v1"`).
    Constant,
    /// Symbol → expression (function-like `#define ADD(a,b)`, parenthesized expressions).
    Macro,
    /// Empty body, non-guard (`#define DEBUG`, `#define ENABLE_FOO`).
    Flag,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawImport {
    pub source: String,
    pub imported_name: String,
    pub alias: Option<String>,
    /// `None` for ordinary import statements; `Some(_)` for C named bindings
    /// (`typedef`, `#define`, `extern`) classified by body shape.
    pub binding_kind: Option<BindingKind>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawRoute {
    pub method: String,
    pub path: String,
    pub handler: Option<String>,
    pub span: (u32, u32, u32, u32),
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawDocumentBlock {
    pub name: String,
    pub is_section: bool,
    pub span: (u32, u32, u32, u32),
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawFrameworkRef {
    pub source_name: String,
    pub target_name: String,
    pub confidence: f32,
    pub reason: String,
    pub span: (u32, u32, u32, u32),
}

/// Primitive type of a schema column or model field.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[rkyv(derive(Debug))]
pub enum SchemaType {
    String,
    Int,
    Float,
    Bool,
    Datetime,
    Json,
    Other,
}

/// Message-bus direction for a call site.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum PubSub {
    Publish,
    Subscribe,
}

/// Origin framework that triggered detection of a `SchemaField` /
/// `EventTopic` / `TxScope`. Closed set, stored as u8 so per-instance cost
/// is 1 byte versus ~24 bytes + heap copy for a `String`. Variants are
/// archive-stable as long as new ones are appended (never reordered);
/// `as_str()` round-trips via `FRAMEWORK_NAMES`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum FrameworkId {
    // ── Schema field origins (T4 family) ──
    Pydantic,
    SqlAlchemy,
    Django,
    Prisma,
    Sqlx,
    TypeScriptInterface,
    // ── Event topic transports (T5 family) ──
    Kafka,
    Sns,
    Sqs,
    RabbitMq,
    EventBridge,
    // ── Transaction scopes (T10 family — values observed in PR #272) ──
    SpringTransactional,
    JpaTransactional,
    DjangoAtomic,
    PonyDbSession,
    // ── Schema field origins (T4-5) ──
    Protobuf,
    // ── Event topic transports (T5 family — continued) ──
    /// Celery distributed task queue — task enqueue model (T5-20).
    /// Distinguished from Kafka (durable log) and Redis pub/sub
    /// (fire-and-forget) so LLMs know task invocations are durable
    /// (broker-backed queue with retry semantics).
    Celery,
    /// Redis pub/sub — fire-and-forget channel model (T5-26/T5-27/T5-28).
    /// Distinguished from Kafka (durable log) and RabbitMQ (queued AMQP)
    /// so LLMs know subscribers MUST be online when publish fires.
    Redis,
    // ── Schema field origins (T4-6) ──
    OpenApi,
    Swagger,
    // ── Fallback for frameworks not yet listed; promote to its own variant
    //    when adding emit support, do not extend silently. ──
    Unknown,
}

pub const FRAMEWORK_NAMES: &[&str] = &[
    "pydantic",
    "sqlalchemy",
    "django",
    "prisma",
    "sqlx",
    "typescript-interface",
    "kafka",
    "sns",
    "sqs",
    "rabbitmq",
    "eventbridge",
    "spring-transactional",
    "jpa-transactional",
    "django-atomic",
    "pony-db-session",
    "protobuf",
    "celery",
    "redis",
    "openapi",
    "swagger",
    "unknown",
];

impl FrameworkId {
    /// Layout-locked with the enum discriminant. Asserted at startup by
    /// `debug_assert_eq!` in tests so a future variant reorder is caught.
    pub const fn as_str(self) -> &'static str {
        FRAMEWORK_NAMES[self as usize]
    }

    /// Decode a u8 (e.g. from a packed bitfield or corrupted archive) into a
    /// FrameworkId. Out-of-range bytes fall back to `Unknown` — preserves
    /// archive read safety when `RawTxScope.packed` is consumed from a
    /// `graph.bin` written by a future version with extra variants.
    ///
    /// Exhaustive `match` instead of bounds-checked `transmute`: adding a new
    /// variant without updating this arm is a compile error, so the
    /// "discriminant ↔ variant" link is enforced by the compiler rather than
    /// by a documented invariant. Modern rustc lowers a 0..N integer match on
    /// a `#[repr(u8)]` enum to a jump table — zero runtime cost vs the
    /// transmute path.
    #[inline]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Pydantic,
            1 => Self::SqlAlchemy,
            2 => Self::Django,
            3 => Self::Prisma,
            4 => Self::Sqlx,
            5 => Self::TypeScriptInterface,
            6 => Self::Kafka,
            7 => Self::Sns,
            8 => Self::Sqs,
            9 => Self::RabbitMq,
            10 => Self::EventBridge,
            11 => Self::SpringTransactional,
            12 => Self::JpaTransactional,
            13 => Self::DjangoAtomic,
            14 => Self::PonyDbSession,
            15 => Self::Protobuf,
            16 => Self::Celery,
            17 => Self::Redis,
            18 => Self::OpenApi,
            19 => Self::Swagger,
            _ => Self::Unknown,
        }
    }
}

/// ORM / schema model field detected at static-analysis time.
///
/// Field-name + owner-class are stored as owned `Box<str>` rather than
/// `StrRef` because per-language parsers run in isolated scopes — the
/// `StringPool` they intern into is dropped before the `LocalGraph` reaches
/// the builder. Owned strings cost an extra 16 B per field but eliminate
/// the dangling-StrRef pre-T4-7 bug entirely. Aligns with `RawNode.name`
/// which is also `String` for the same reason.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawSchemaField {
    pub name: Box<str>,
    pub type_class: SchemaType,
    pub owner_class: Box<str>,
    pub framework: FrameworkId,
    pub span: (u32, u32, u32, u32),
}

/// Message-bus publish/subscribe call site.
///
/// `topic_literal` and `enclosing_fn` are owned `Box<str>` so the struct is
/// self-contained after the per-file parse scope drops. Previous
/// `StrRef`-based layout required callers to carry a dropped local pool,
/// making post-process promotion (T5-33) impossible without leaking the
/// pool into `LocalGraph`. `Box<str>` matches `RawSchemaField`'s pattern.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawEventTopic {
    /// `None` when the topic string is dynamic (variable arg); emitter
    /// records a `BlindSpot` instead. Already canonicalised by the
    /// per-language detector via `event_topic::normalize::canonicalize`.
    pub topic_literal: Option<Box<str>>,
    pub direction: PubSub,
    pub lib: FrameworkId,
    /// Name of the enclosing function/method at the call site.
    /// Empty string when the detector's producer-capture is absent.
    pub enclosing_fn: Box<str>,
    pub span: (u32, u32, u32, u32),
}

/// Transactional scope boundary (e.g. `@Transactional`, `atomic()`).
///
/// Packed: high 24 bits = node index into `LocalGraph.nodes` (the
/// Method / Function / Constructor whose body the boundary scopes),
/// low 8 bits = `FrameworkId` discriminant. 4 bytes total — 7× denser
/// than the prior (StrRef + FrameworkId + span) shape because the
/// enclosing node already carries `name`, `span`, and `decorators`.
/// Resolve via `nodes[scope.node_idx() as usize]`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
pub struct RawTxScope {
    /// Bit-packed `(node_idx << 8) | framework_id_u8`. Visibility is
    /// `pub(crate)` so out-of-crate consumers must go through the
    /// `node_idx()` / `framework()` accessor methods — keeps the layout
    /// free to change without breaking external callers.
    pub(crate) packed: u32,
}

impl RawTxScope {
    /// Largest node index that fits in the high 24 bits.
    pub const NODE_IDX_MAX: u32 = (1 << 24) - 1;

    /// Construct a packed scope. `debug_assert!` enforces the u24 limit on
    /// `node_idx` — files with more than 16 M symbol nodes are not realistic
    /// for static analysis and would indicate a generated-code fixture
    /// pathology.
    #[inline]
    pub fn new(node_idx: u32, framework: FrameworkId) -> Self {
        debug_assert!(
            node_idx <= Self::NODE_IDX_MAX,
            "RawTxScope::new: node_idx {} exceeds u24 limit",
            node_idx
        );
        Self {
            packed: (node_idx << 8) | (framework as u32),
        }
    }

    #[inline]
    pub const fn node_idx(self) -> u32 {
        self.packed >> 8
    }

    #[inline]
    pub const fn framework(self) -> FrameworkId {
        FrameworkId::from_u8((self.packed & 0xFF) as u8)
    }
}

impl ArchivedRawTxScope {
    #[inline]
    pub fn node_idx(&self) -> u32 {
        self.packed.to_native() >> 8
    }

    #[inline]
    pub fn framework(&self) -> FrameworkId {
        FrameworkId::from_u8((self.packed.to_native() & 0xFF) as u8)
    }
}

/// Reflection-style fan-out reference: a single call site whose target cannot
/// be uniquely picked at static-analysis time, but where the analyzer can
/// enumerate the candidate set. The builder emits one `References` edge per
/// candidate with confidence `base_confidence / sqrt(N)` (floored at 0.1).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawFanoutRef {
    pub source_name: String,
    pub candidates: Vec<String>,
    pub base_confidence: f32,
    pub reason: String,
    pub span: (u32, u32, u32, u32),
}

/// Truly unresolvable code pattern (eval/exec/dynamic-import/cross-object
/// reflection/...). Unlike `RawFanoutRef`, candidates cannot even be
/// enumerated — the analyzer just records "this is a blind spot" so
/// downstream LLM tooling can flag the location for manual inspection.
///
/// Carries `file_path` directly (unlike other Raw* types whose file is
/// implicit in their owning `LocalGraph`) because blind spots are
/// passed through to graph-level metadata where the source file must
/// remain identifiable after the LocalGraph is consumed.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct BlindSpot {
    pub kind: String,
    #[rkyv(with = rkyv::with::AsString)]
    pub file_path: PathBuf,
    pub span: (u32, u32, u32, u32),
    pub hint: String,
    /// True iff the BlindSpot originates from a file classified as test
    /// scaffolding (`is_test_path(file_path)`). Populated by each
    /// language parser at emission time; passes through to
    /// `graph::BlindSpotRecord.is_test`. Verdict layer uses this to
    /// suppress test-region noise on prod-refactor PRs.
    pub is_test: bool,
}

/// Per-call-site dispatch annotation produced by the per-language indirect-
/// dispatch detector. Identifies which entry in `RawNode.calls` (by caller
/// name + zero-based index) is non-direct so the builder can emit a
/// `graph::CallMeta` entry keyed on the resulting `Edge` index.
///
/// Only non-direct calls get a `RawCallMeta` — direct calls are the default
/// and are not annotated (saves space; sparse population contract matches
/// `ZeroCopyGraph.call_metas`).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawCallMeta {
    /// Name of the enclosing `RawNode` (caller) that owns the call.
    pub caller_name: String,
    /// Span of the enclosing caller node. Combined with `call_index` to
    /// uniquely identify same-name functions/methods in one file.
    pub caller_span: (u32, u32, u32, u32),
    /// Zero-based index into `RawNode.calls` for that caller.
    pub call_index: u32,
    /// Packed flags — same bit layout as `graph::CallMeta::flags`.
    /// Populated by the per-language detector; `FLAG_DIRECT` is always clear
    /// (this struct only exists for non-direct calls).
    pub flags: u8,
    /// Dispatch type string as it appears in source (e.g. `"dyn Handler"`,
    /// `"void(*)(int)"`, `"Box<dyn Trait>"`). Empty string when not available.
    pub dispatch_type: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct LocalGraph {
    #[rkyv(with = rkyv::with::AsString)]
    pub file_path: PathBuf,
    pub content_hash: [u8; 8],
    pub nodes: Vec<RawNode>,
    pub documents: Vec<RawDocumentBlock>,
    pub imports: Vec<RawImport>,
    pub routes: Vec<RawRoute>,
    pub framework_refs: Vec<RawFrameworkRef>,
    pub fanout_refs: Vec<RawFanoutRef>,
    pub blind_spots: Vec<BlindSpot>,
    /// Side tables for T-phase detectors. `None` until the corresponding
    /// detector populates them — saves 16 bytes/field versus an always-empty
    /// `Vec` and skips an archived length marker in `graph.bin` for files
    /// with no schema / event / transaction surface (i.e. the majority).
    pub schema_fields: Option<Box<[RawSchemaField]>>,
    pub event_topics: Option<Box<[RawEventTopic]>>,
    pub tx_scopes: Option<Box<[RawTxScope]>>,
    /// Indirect-dispatch annotations for individual call sites. Sparse:
    /// only non-direct calls (fn-pointer / vtable / callback / dyn-trait)
    /// have entries. The builder promotes these to `ZeroCopyGraph.call_metas`
    /// once `Edge` indices are known.
    pub call_metas: Vec<RawCallMeta>,
    /// Per-function metadata captured during parsing. Paired with `nodes` by
    /// span at build time; only populated for `Function`/`Method`/`Constructor`
    /// nodes. Empty for languages not yet covered by Phase 1/2.
    pub raw_function_metas: Vec<RawFunctionMeta>,
}

impl Default for LocalGraph {
    fn default() -> Self {
        Self {
            file_path: PathBuf::new(),
            content_hash: [0; 8],
            nodes: Vec::new(),
            documents: Vec::new(),
            imports: Vec::new(),
            routes: Vec::new(),
            framework_refs: Vec::new(),
            fanout_refs: Vec::new(),
            blind_spots: Vec::new(),
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: Vec::new(),
            raw_function_metas: Vec::new(),
        }
    }
}
