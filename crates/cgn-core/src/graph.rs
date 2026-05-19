use crate::pool::StrRef;
use rkyv::{Archive, Deserialize, Serialize};

/// Magic bytes at the head of every `graph.bin`. Used by the reader to
/// reject non-cgn files (or files truncated below the header length)
/// before rkyv attempts a structural cast.
pub const GRAPH_MAGIC: [u8; 8] = *b"CGN-RS\0\0";

/// On-disk graph format version. Bump whenever `ZeroCopyGraph`'s field
/// layout changes in a way that would make older binaries unreadable by
/// the new reader (or vice-versa). The reader refuses any version it
/// does not recognize, so a stale CLI does not segfault on a fresh
/// `graph.bin` and a fresh CLI does not silently misinterpret old data.
pub const GRAPH_FORMAT_VERSION: u32 = 4;

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
            _ => Err(()),
        }
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
pub enum NodeKind {
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
    /// `cgn_analyzer::entry_points`. References the underlying
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
    /// a symbol callers reach for directly, but `cgn inspect` needs it
    /// to enumerate associated functions per type.
    Impl,
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

    /// Static variant name. Used by Pass 1 UID construction (`"<Kind>:<path>:<name>"`)
    /// where `write!(.., "{:?}", kind)` would otherwise go through `fmt`
    /// dispatch per node (~300k on `.sample_repo`). Matches the variant
    /// identifier exactly, so existing UID strings stay byte-stable.
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
        }
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
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
    /// `cgn_analyzer::fetch_shape`.
    Fetches,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct Node {
    pub uid: StrRef,
    pub name: StrRef,
    pub file_idx: u32,
    pub kind: NodeKind,
    pub span: (u32, u32, u32, u32), // start_line, start_col, end_line, end_col
    pub community_id: u16,          // 0 = unassigned
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
/// `cgn context` / `cgn analyze` can surface blind spots to the LLM,
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
    pub name_index: Vec<u32>,

    /// Boundary index: `nodes[process_start..]` are all `NodeKind::Process`.
    /// For node_idx >= process_start, `process_k = node_idx - process_start`
    /// and its trace lives in `traces_data[traces_offsets[k]..traces_offsets[k+1]]`.
    pub process_start: u32,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::StringPool;
    use rkyv::rancor::Error;

    #[test]
    fn test_serialize_deserialize_graph() {
        let mut pool = StringPool::new();
        let name_ref = pool.add("main");
        let uid_ref = pool.add("Function:src/main.ts:main");

        let graph = ZeroCopyGraph {
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
                uid: uid_ref,
                name: name_ref,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (1, 0, 5, 0),
                community_id: 0,
            }],
            edges: vec![],
            out_offsets: vec![0, 0],
            in_offsets: vec![0, 0],
            in_edge_idx: vec![],
            name_index: vec![],
            process_start: 1,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
        };

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
    }
}
