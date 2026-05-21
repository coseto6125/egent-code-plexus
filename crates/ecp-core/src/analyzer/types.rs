use crate::graph::NodeKind;
use crate::pool::StrRef;
use rkyv::{Archive, Deserialize, Serialize};
use std::path::PathBuf;

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
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
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
    "unknown",
];

impl FrameworkId {
    /// Layout-locked with the enum discriminant. Asserted at startup by
    /// `debug_assert_eq!` in tests so a future variant reorder is caught.
    pub const fn as_str(self) -> &'static str {
        FRAMEWORK_NAMES[self as usize]
    }
}

/// ORM / schema model field detected at static-analysis time.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawSchemaField {
    pub name: StrRef,
    pub type_class: SchemaType,
    pub owner_class: StrRef,
    pub framework: FrameworkId,
    pub span: (u32, u32, u32, u32),
}

/// Message-bus publish/subscribe call site.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawEventTopic {
    /// None = dynamic topic, upstream emits BlindSpot
    pub topic_literal: Option<StrRef>,
    pub direction: PubSub,
    pub lib: FrameworkId,
    pub enclosing_fn: StrRef,
    pub span: (u32, u32, u32, u32),
}

/// Transactional scope boundary (e.g. `@Transactional`, `atomic()`).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawTxScope {
    pub enclosing_fn: StrRef,
    pub source_pattern: FrameworkId,
    pub span: (u32, u32, u32, u32),
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
        }
    }
}
