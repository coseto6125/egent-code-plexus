//! C5 routing result: one definition of how a repo-relative path becomes a
//! file format, a provider, a language, a capability
//! set and a size cap. Design note: `docs/specs/contracts/C5-routing.md`.
//!
//! Every per-language fact is an exhaustive `match` on [`Language`] with no
//! wildcard arm, so a new language does not compile until each fact is
//! decided. The extension defaults come from [`Language::extensions`], so the
//! lookup and its reverse are one table.
//!
//! The parse pipeline, the cold walk, the provider-set selection of every
//! parse path and several analyzer passes route through [`FileRouter`]. The
//! design note lists the readers that still decide on their own under "Today".

use crate::config::DEFAULT_MAX_FILE_BYTES;
use crate::file_category::Language as LegacyLanguage;
use rustc_hash::FxHashMap;
use std::sync::LazyLock;

/// Declares a fieldless `#[repr(u8)]` enum and its `ALL` list from one body,
/// so `ALL` cannot miss a variant.
macro_rules! listed_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($(#[$vmeta:meta])* $variant:ident = $disc:literal,)+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum $name {
            $($(#[$vmeta])* $variant = $disc,)+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant,)+];
        }
    };
}

listed_enum! {
    /// The language of a routed file: one variant per parser provider that a
    /// path can reach. The discriminants are append-only, because a slice may
    /// store this value per file (design note, "Stored data").
    pub enum Language {
        TypeScript = 0,
        JavaScript = 1,
        Python = 2,
        Java = 3,
        Kotlin = 4,
        CSharp = 5,
        Go = 6,
        Rust = 7,
        Php = 8,
        Ruby = 9,
        Swift = 10,
        C = 11,
        /// Also every `.h` header: C++ parsing is a near-superset of C.
        Cpp = 12,
        Dart = 13,
        Bash = 14,
        Lua = 15,
        Crystal = 16,
        Move = 17,
        Solidity = 18,
        Dockerfile = 19,
        Nim = 20,
        Hcl = 21,
        Sql = 22,
        Vyper = 23,
        Verilog = 24,
        Cairo = 25,
        Zig = 26,
        /// A single-file component. Its `<script>` language is decided by the
        /// parse ([`Language::embedded_scripts`]).
        Vue = 27,
        Astro = 28,
        Svelte = 29,
        Protobuf = 30,
        DockerCompose = 31,
        GitHubActions = 32,
        /// YAML whose content carries Kubernetes markers.
        Kubernetes = 33,
        /// YAML or JSON whose content carries an `openapi:` or `swagger:` marker.
        OpenApi = 34,
        /// YAML that no content gate before it accepted.
        Yaml = 35,
    }
}

listed_enum! {
    /// A capability that a reader dispatches on by language. A language that
    /// lacks one must make the reader say so, never return an empty answer.
    pub enum Capability {
        /// `ecp pattern` matches a metavariable pattern in the file.
        PatternFinder = 0,
        /// `ecp flow` lowers the file.
        Flow = 1,
        /// The index extracts response shapes from HTTP handlers.
        FetchShapes = 2,
        /// The index extracts HTTP client call sites (`Fetches` sources).
        FetchConsumer = 3,
        /// The parser emits function meta, which sets `is_test` on callers.
        FunctionMeta = 4,
        /// The parser marks non-direct calls.
        IndirectDispatch = 5,
        /// `ecp tool-map` reads the file's package imports.
        ToolMapImports = 6,
        /// The JS and TS lexical scope index covers the file.
        LexicalIndex = 7,
    }
}

listed_enum! {
    /// The ordered path rules. [`FileRouter::route`] tries them in this order,
    /// and the first rule that decides wins.
    pub enum PathRule {
        /// A component named `__brick__` (mason templates). Excludes the path.
        TemplateDir = 0,
        /// A component that holds `{{` or `}}` (mason interpolation).
        /// Excludes the path.
        TemplateInterpolation = 1,
        /// Basename `Dockerfile` or `dockerfile`.
        DockerfileName = 2,
        /// Basename `docker-compose.yml`, `docker-compose.yaml`, `compose.yml`
        /// or `compose.yaml`.
        ComposeName = 3,
        /// Extension `yml` or `yaml` under adjacent `.github/workflows`
        /// components.
        GitHubWorkflow = 4,
        /// Any other `yml` or `yaml` file: the content decides.
        YamlContent = 5,
        /// A `json` file: the content decides.
        JsonContent = 6,
        /// The extension defaults of [`Language::extensions`].
        Extension = 7,
    }
}

/// The candidates of a YAML file, in content-gate order.
pub const YAML_CANDIDATES: &[Language] = &[Language::Kubernetes, Language::OpenApi, Language::Yaml];

/// The candidates of a JSON file. A JSON file that the OpenAPI gate rejects
/// has no language after parse and no graph.
pub const JSON_CANDIDATES: &[Language] = &[Language::OpenApi];

impl Language {
    /// The variant whose `#[repr(u8)]` discriminant is `byte`, the inverse of
    /// `language as u8`. `None` for a byte that names no variant.
    pub fn from_discriminant(byte: u8) -> Option<Self> {
        Self::ALL
            .get(byte as usize)
            .copied()
            .filter(|language| *language as u8 == byte)
    }

    /// The 14 mainstream languages of the repo's coverage rule.
    pub const MAINSTREAM: [Language; 14] = [
        Self::TypeScript,
        Self::JavaScript,
        Self::Python,
        Self::Java,
        Self::Kotlin,
        Self::CSharp,
        Self::Go,
        Self::Rust,
        Self::Php,
        Self::Ruby,
        Self::Swift,
        Self::C,
        Self::Cpp,
        Self::Dart,
    ];

    /// The provider name, as `PROVIDER_CONSTRUCTORS` registers it. One
    /// language has one provider.
    pub const fn provider(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Java => "java",
            Self::Kotlin => "kotlin",
            Self::CSharp => "c_sharp",
            Self::Go => "go",
            Self::Rust => "rust",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Swift => "swift",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Dart => "dart",
            Self::Bash => "bash",
            Self::Lua => "lua",
            Self::Crystal => "crystal",
            Self::Move => "move",
            Self::Solidity => "solidity",
            Self::Dockerfile => "dockerfile",
            Self::Nim => "nim",
            Self::Hcl => "hcl",
            Self::Sql => "sql",
            Self::Vyper => "vyper",
            Self::Verilog => "verilog",
            Self::Cairo => "cairo",
            Self::Zig => "zig",
            Self::Vue => "vue",
            Self::Astro => "astro",
            Self::Svelte => "svelte",
            Self::Protobuf => "protobuf",
            Self::DockerCompose => "docker-compose",
            Self::GitHubActions => "github-actions",
            Self::Kubernetes => "kubernetes",
            Self::OpenApi => "openapi",
            Self::Yaml => "yaml",
        }
    }

    /// The language whose provider is `name`.
    pub fn from_provider(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|language| language.provider() == name)
    }

    /// The extension defaults, case-sensitive, without the dot. The first
    /// entry is the canonical extension. A language reached only by a path
    /// rule has none. No extension belongs to two languages.
    pub const fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::TypeScript => &["ts", "tsx"],
            Self::JavaScript => &["js", "jsx", "mjs", "cjs"],
            Self::Python => &["py", "pyi"],
            Self::Java => &["java"],
            Self::Kotlin => &["kt", "kts"],
            Self::CSharp => &["cs"],
            Self::Go => &["go"],
            Self::Rust => &["rs"],
            Self::Php => &["php"],
            Self::Ruby => &["rb"],
            Self::Swift => &["swift"],
            Self::C => &["c"],
            Self::Cpp => &["cpp", "hpp", "cc", "hh", "cxx", "hxx", "h"],
            Self::Dart => &["dart"],
            Self::Bash => &["sh", "bash"],
            Self::Lua => &["lua", "luau"],
            Self::Crystal => &["cr"],
            Self::Move => &["move"],
            Self::Solidity => &["sol"],
            Self::Dockerfile => &["dockerfile"],
            Self::Nim => &["nim"],
            Self::Hcl => &["tf", "tfvars", "hcl"],
            Self::Sql => &["sql"],
            Self::Vyper => &["vy"],
            Self::Verilog => &["v", "sv", "vh", "svh"],
            Self::Cairo => &["cairo"],
            Self::Zig => &["zig"],
            Self::Vue => &["vue"],
            Self::Astro => &["astro"],
            Self::Svelte => &["svelte"],
            Self::Protobuf => &["proto"],
            Self::DockerCompose
            | Self::GitHubActions
            | Self::Kubernetes
            | Self::OpenApi
            | Self::Yaml => &[],
        }
    }

    /// The language whose extension defaults hold `ext`.
    pub fn from_extension(ext: &str) -> Option<Self> {
        // Every walked path and every porcelain entry routes through here, so
        // a scan over every language's extension list would run per path.
        static BY_EXT: LazyLock<FxHashMap<&'static str, Language>> = LazyLock::new(|| {
            Language::ALL
                .iter()
                .flat_map(|&language| {
                    language
                        .extensions()
                        .iter()
                        .map(move |&ext| (ext, language))
                })
                .collect()
        });
        BY_EXT.get(ext).copied()
    }

    /// The display name in answers. Equals today's
    /// `file_category::Language::as_str` for every language that has a
    /// variant there.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Python => "Python",
            Self::Java => "Java",
            Self::Kotlin => "Kotlin",
            Self::CSharp => "CSharp",
            Self::Go => "Go",
            Self::Rust => "Rust",
            Self::Php => "PHP",
            Self::Ruby => "Ruby",
            Self::Swift => "Swift",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Dart => "Dart",
            Self::Bash => "Bash",
            Self::Lua => "Lua",
            Self::Crystal => "Crystal",
            Self::Move => "Move",
            Self::Solidity => "Solidity",
            Self::Dockerfile => "Dockerfile",
            Self::Nim => "Nim",
            Self::Hcl => "HCL",
            Self::Sql => "SQL",
            Self::Vyper => "Vyper",
            Self::Verilog => "Verilog",
            Self::Cairo => "Cairo",
            Self::Zig => "Zig",
            Self::Vue => "Vue",
            Self::Astro => "Astro",
            Self::Svelte => "Svelte",
            Self::Protobuf => "Protobuf",
            Self::DockerCompose => "DockerCompose",
            Self::GitHubActions => "GitHubActions",
            Self::Kubernetes => "Kubernetes",
            Self::OpenApi => "OpenAPI",
            Self::Yaml => "YAML",
        }
    }

    /// The capabilities that hold for every file of this language. Today's
    /// sources, per capability, are in the design note.
    pub const fn capabilities(self) -> CapabilitySet {
        use Capability as K;
        match self {
            Self::TypeScript | Self::JavaScript => CapabilitySet::of(&[
                K::PatternFinder,
                K::Flow,
                K::FetchShapes,
                K::FetchConsumer,
                K::FunctionMeta,
                K::IndirectDispatch,
                K::ToolMapImports,
                K::LexicalIndex,
            ]),
            Self::Python => CapabilitySet::of(&[
                K::PatternFinder,
                K::Flow,
                K::FetchConsumer,
                K::FunctionMeta,
                K::IndirectDispatch,
                K::ToolMapImports,
            ]),
            Self::Php => {
                CapabilitySet::of(&[K::PatternFinder, K::Flow, K::FetchShapes, K::FunctionMeta])
            }
            Self::Rust => CapabilitySet::of(&[
                K::PatternFinder,
                K::FunctionMeta,
                K::IndirectDispatch,
                K::ToolMapImports,
            ]),
            Self::Go => CapabilitySet::of(&[K::PatternFinder, K::FunctionMeta, K::ToolMapImports]),
            Self::C | Self::Cpp => {
                CapabilitySet::of(&[K::PatternFinder, K::FunctionMeta, K::IndirectDispatch])
            }
            Self::Java | Self::Kotlin | Self::CSharp | Self::Ruby | Self::Swift => {
                CapabilitySet::of(&[K::PatternFinder, K::FunctionMeta])
            }
            Self::Dart => CapabilitySet::of(&[K::FunctionMeta]),
            Self::Bash
            | Self::Lua
            | Self::Crystal
            | Self::Move
            | Self::Solidity
            | Self::Nim
            | Self::Hcl
            | Self::Sql
            | Self::Vyper
            | Self::Verilog
            | Self::Cairo
            | Self::Zig
            | Self::Dockerfile
            | Self::Vue
            | Self::Astro
            | Self::Svelte
            | Self::Protobuf
            | Self::DockerCompose
            | Self::GitHubActions
            | Self::Kubernetes
            | Self::OpenApi
            | Self::Yaml => CapabilitySet::EMPTY,
        }
    }

    /// The script languages a file of this language may embed. The parse
    /// reports which one the file uses.
    pub const fn embedded_scripts(self) -> &'static [Language] {
        match self {
            Self::Vue | Self::Astro | Self::Svelte => &[Self::TypeScript, Self::JavaScript],
            Self::TypeScript
            | Self::JavaScript
            | Self::Python
            | Self::Java
            | Self::Kotlin
            | Self::CSharp
            | Self::Go
            | Self::Rust
            | Self::Php
            | Self::Ruby
            | Self::Swift
            | Self::C
            | Self::Cpp
            | Self::Dart
            | Self::Bash
            | Self::Lua
            | Self::Crystal
            | Self::Move
            | Self::Solidity
            | Self::Dockerfile
            | Self::Nim
            | Self::Hcl
            | Self::Sql
            | Self::Vyper
            | Self::Verilog
            | Self::Cairo
            | Self::Zig
            | Self::Protobuf
            | Self::DockerCompose
            | Self::GitHubActions
            | Self::Kubernetes
            | Self::OpenApi
            | Self::Yaml => &[],
        }
    }

    /// The value today's `file_category::Language` holds for this language.
    /// `Unknown` where today has no variant. The path-level differences are
    /// in the design note ("Today").
    pub const fn to_legacy(self) -> LegacyLanguage {
        match self {
            Self::TypeScript => LegacyLanguage::TypeScript,
            Self::JavaScript => LegacyLanguage::JavaScript,
            Self::Python => LegacyLanguage::Python,
            Self::Java => LegacyLanguage::Java,
            Self::Kotlin => LegacyLanguage::Kotlin,
            Self::CSharp => LegacyLanguage::CSharp,
            Self::Go => LegacyLanguage::Go,
            Self::Rust => LegacyLanguage::Rust,
            Self::Php => LegacyLanguage::Php,
            Self::Ruby => LegacyLanguage::Ruby,
            Self::Swift => LegacyLanguage::Swift,
            Self::C => LegacyLanguage::C,
            Self::Cpp => LegacyLanguage::Cpp,
            Self::Dart => LegacyLanguage::Dart,
            Self::Bash => LegacyLanguage::Bash,
            Self::Lua => LegacyLanguage::Lua,
            Self::Crystal => LegacyLanguage::Crystal,
            Self::Move => LegacyLanguage::Move,
            Self::Solidity => LegacyLanguage::Solidity,
            Self::Dockerfile => LegacyLanguage::Dockerfile,
            Self::Nim => LegacyLanguage::Nim,
            Self::Hcl => LegacyLanguage::Hcl,
            Self::Sql => LegacyLanguage::Sql,
            Self::Vyper => LegacyLanguage::Vyper,
            Self::Verilog => LegacyLanguage::Verilog,
            Self::Cairo => LegacyLanguage::Cairo,
            Self::Zig => LegacyLanguage::Zig,
            Self::DockerCompose => LegacyLanguage::DockerCompose,
            Self::GitHubActions => LegacyLanguage::GitHubActions,
            Self::Kubernetes => LegacyLanguage::Kubernetes,
            Self::Yaml => LegacyLanguage::Yaml,
            Self::Vue | Self::Astro | Self::Svelte | Self::Protobuf | Self::OpenApi => {
                LegacyLanguage::Unknown
            }
        }
    }
}

/// A set of [`Capability`] values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CapabilitySet(u16);

impl CapabilitySet {
    pub const EMPTY: Self = Self(0);
    pub const FULL: Self = Self::of(Capability::ALL);

    pub const fn of(capabilities: &[Capability]) -> Self {
        let mut bits = 0u16;
        let mut i = 0;
        while i < capabilities.len() {
            bits |= 1u16 << (capabilities[i] as u16);
            i += 1;
        }
        Self(bits)
    }

    pub const fn contains(self, capability: Capability) -> bool {
        self.0 & (1u16 << (capability as u16)) != 0
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .iter()
            .copied()
            .filter(move |capability| self.contains(*capability))
    }
}

/// The syntax the path names before any content is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileFormat {
    /// Program, build or schema text in the language's own grammar.
    Code,
    Yaml,
    Json,
}

/// The grammar variant of a language that has two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Dialect {
    Standard = 0,
    /// `.tsx`: the TSX grammar of TypeScript.
    Tsx = 1,
}

/// The part of a parse's input that the path decides: the rule, the dialect
/// and the fixed language (`0xFF` for a sniffed route). An L1 fragment key
/// hashes these bytes with the file content, so two paths with identical
/// bytes and different routes never share a fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RouteKey([u8; 3]);

impl RouteKey {
    /// The byte that stands for "no fixed language".
    pub const SNIFFED: u8 = u8::MAX;

    pub const fn to_bytes(self) -> [u8; 3] {
        self.0
    }
}

/// How the parse picks the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// The path decides the language.
    Fixed(Language),
    /// The content decides. The parse tries each candidate's provider in
    /// order, and the first content gate that accepts names the language
    /// after parse. Never empty, and no candidate is mainstream.
    Sniffed(&'static [Language]),
}

/// Where the effective size cap came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeCapSource {
    /// `ECP_MAX_FILE_BYTES`.
    Env,
    /// `[index] max_file_bytes` in `<repo>/.ecp/config.toml`.
    RepoConfig,
    /// [`DEFAULT_MAX_FILE_BYTES`].
    BuiltIn,
}

/// The one per-file byte cap of every parse path: the cold index, the
/// incremental reanalyze, the overlay writer and `impact --baseline` each
/// resolve it from the repo root with `config::resolve_size_cap`. A file over
/// the cap is routed but not parsed, and the parse returns a graph that
/// records it as omitted (`file-too-large`).
///
/// The dirty manifest records `bytes` (`DirtyEvents::size_cap_bytes`), and a
/// manifest under another cap is stale as a whole; the view cache key hashes
/// the accepted fragments, so it follows. `graph.bin` does not record its
/// cap: a cap change reaches a clean file at the next rebuild. Every cache
/// compares `bytes`, never `source`: one cap from two sources parses the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SizeCap {
    bytes: u64,
    source: SizeCapSource,
}

impl SizeCap {
    pub const BUILT_IN: Self = Self {
        bytes: DEFAULT_MAX_FILE_BYTES,
        source: SizeCapSource::BuiltIn,
    };

    /// Precedence: a parseable `env` value, then `repo_config`, then the
    /// built-in default. A malformed `env` value falls through.
    pub fn resolve(env: Option<&str>, repo_config: Option<u64>) -> Self {
        if let Some(bytes) = env.and_then(|value| value.parse::<u64>().ok()) {
            return Self {
                bytes,
                source: SizeCapSource::Env,
            };
        }
        match repo_config {
            Some(bytes) => Self {
                bytes,
                source: SizeCapSource::RepoConfig,
            },
            None => Self::BUILT_IN,
        }
    }

    pub const fn bytes(self) -> u64 {
        self.bytes
    }

    pub const fn source(self) -> SizeCapSource {
        self.source
    }

    /// A file of exactly `bytes` is admitted.
    pub const fn admits(self, len: u64) -> bool {
        len <= self.bytes
    }
}

/// The routing result of one path that a rule routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRoute {
    rule: PathRule,
    format: FileFormat,
    dispatch: Dispatch,
    dialect: Dialect,
    size_cap: SizeCap,
}

impl FileRoute {
    /// The rule that decided.
    pub const fn rule(&self) -> PathRule {
        self.rule
    }

    pub const fn format(&self) -> FileFormat {
        self.format
    }

    pub const fn dispatch(&self) -> Dispatch {
        self.dispatch
    }

    pub const fn dialect(&self) -> Dialect {
        self.dialect
    }

    pub const fn size_cap(&self) -> SizeCap {
        self.size_cap
    }

    /// The fragment identity part of this route. Equal for two routes only
    /// when the rule, the dialect and the fixed language are equal.
    pub const fn key(&self) -> RouteKey {
        let language = match self.dispatch {
            Dispatch::Fixed(language) => language as u8,
            Dispatch::Sniffed(_) => RouteKey::SNIFFED,
        };
        RouteKey([self.rule as u8, self.dialect as u8, language])
    }

    /// The language when the path decides it. `None` for a sniffed route:
    /// read [`ParsedLanguage`] after the parse instead.
    pub const fn fixed_language(&self) -> Option<Language> {
        match self.dispatch {
            Dispatch::Fixed(language) => Some(language),
            Dispatch::Sniffed(_) => None,
        }
    }

    /// The languages the parse may decide, in try order.
    pub fn candidates(&self) -> &[Language] {
        match self.dispatch {
            Dispatch::Fixed(ref language) => std::slice::from_ref(language),
            Dispatch::Sniffed(candidates) => candidates,
        }
    }

    /// The providers the parse may run, in try order. A generic YAML lists no
    /// `yaml` provider: its graph waits on C5 open point 6 (a node-count
    /// measurement), so no reader builds a provider that the parse never runs.
    pub fn providers(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.candidates()
            .iter()
            .filter(|language| **language != Language::Yaml)
            .map(|language| language.provider())
    }

    /// The capabilities that hold before the parse: those of every candidate.
    pub fn capabilities_before_parse(&self) -> CapabilitySet {
        self.candidates()
            .iter()
            .fold(CapabilitySet::FULL, |set, language| {
                set.intersection(language.capabilities())
            })
    }

    /// The `file_category::Language` value of a file on this route: the fixed
    /// language's [`Language::to_legacy`]. A sniffed route has no language
    /// before the parse, so it gives the value the path alone gave before
    /// the content gates: `Yaml` for a YAML file, `Unknown` for a JSON file.
    pub const fn to_legacy(&self) -> LegacyLanguage {
        match (self.dispatch, self.format) {
            (Dispatch::Fixed(language), _) => language.to_legacy(),
            (Dispatch::Sniffed(_), FileFormat::Yaml) => LegacyLanguage::Yaml,
            (Dispatch::Sniffed(_), FileFormat::Json | FileFormat::Code) => LegacyLanguage::Unknown,
        }
    }

    /// The language after parse. `accepted` is the candidate whose content
    /// gate accepted; `script` is the embedded script language the parser
    /// read, if the file has one.
    pub fn after_parse(
        &self,
        accepted: Language,
        script: Option<Language>,
    ) -> Result<ParsedLanguage, AfterParseError> {
        if !self.candidates().contains(&accepted) {
            return Err(AfterParseError::NotACandidate { accepted });
        }
        let resolution = match script {
            None => accepted,
            Some(script) if accepted.embedded_scripts().contains(&script) => script,
            Some(script) => {
                return Err(AfterParseError::ScriptNotEmbeddable {
                    file: accepted,
                    script,
                })
            }
        };
        Ok(ParsedLanguage {
            file: accepted,
            resolution,
        })
    }
}

/// The outcome of routing one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteOutcome {
    Routed(FileRoute),
    /// A path rule rejects the path. It is not source.
    Excluded(PathRule),
    /// No rule routes the path. It is not source.
    Unrouted,
}

impl RouteOutcome {
    pub const fn route(&self) -> Option<&FileRoute> {
        match self {
            Self::Routed(route) => Some(route),
            Self::Excluded(_) | Self::Unrouted => None,
        }
    }
}

/// The language facts of a parsed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParsedLanguage {
    file: Language,
    resolution: Language,
}

impl ParsedLanguage {
    /// The language whose provider parsed the file. Tool choice (pattern
    /// grammar, capability set) reads this value.
    pub const fn file(self) -> Language {
        self.file
    }

    /// The language the file's code follows: the embedded script language of
    /// a single-file component, else [`Self::file`]. Call normalization,
    /// import binding, owner rules, global admission and the C2 ecosystem
    /// lookup (`contracts::workspace::Ecosystem::for_language`) read this
    /// value.
    pub const fn resolution(self) -> Language {
        self.resolution
    }

    /// The one compatibility rule of the global (Tier-3) tier: a caller
    /// admits a candidate of the same resolution language. The vendor
    /// barrier stays in `file_category::FileMeta`.
    pub fn admits_global(self, candidate: ParsedLanguage) -> bool {
        self.resolution == candidate.resolution
    }

    /// The stored form of "no `ParsedLanguage`": `0xFF` names no language
    /// (`Language` has fewer than 255 variants, see `RouteKey::SNIFFED`).
    pub const ABSENT: [u8; 2] = [RouteKey::SNIFFED, RouteKey::SNIFFED];

    /// The stored form (`graph.bin` `File::parsed_language`): the
    /// discriminants of [`Self::file`] and [`Self::resolution`].
    pub const fn to_bytes(self) -> [u8; 2] {
        [self.file as u8, self.resolution as u8]
    }

    /// [`Self::to_bytes`] of `parsed`, or [`Self::ABSENT`].
    pub fn encode(parsed: Option<Self>) -> [u8; 2] {
        parsed.map_or(Self::ABSENT, Self::to_bytes)
    }

    /// The inverse of [`Self::encode`]. `None` for [`Self::ABSENT`], for a
    /// byte that names no language, and for a pair `after_parse` never
    /// makes: a resolution language other than the file language or one of
    /// its embedded scripts.
    pub fn from_bytes([file, resolution]: [u8; 2]) -> Option<Self> {
        let file = Language::from_discriminant(file)?;
        let resolution = Language::from_discriminant(resolution)?;
        (resolution == file || file.embedded_scripts().contains(&resolution))
            .then_some(Self { file, resolution })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterParseError {
    /// The accepted language is not a candidate of the route.
    NotACandidate { accepted: Language },
    /// The file's language embeds no script of that language.
    ScriptNotEmbeddable { file: Language, script: Language },
}

/// Routes repo-relative paths with one effective size cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRouter {
    size_cap: SizeCap,
}

impl FileRouter {
    /// The router of a reader that asks only which rule, format or provider a
    /// path reaches. The cap does not decide routing, so any cap answers alike.
    pub const PATH_ONLY: Self = Self::new(SizeCap::BUILT_IN);

    pub const fn new(size_cap: SizeCap) -> Self {
        Self { size_cap }
    }

    /// Route a repo-relative path. `/` and `\` both separate components.
    pub fn route(&self, rel_path: &str) -> RouteOutcome {
        let parts = PathParts::new(rel_path);
        for &rule in PathRule::ALL {
            match rule.decide(&parts) {
                Some(Decision::Exclude) => return RouteOutcome::Excluded(rule),
                Some(Decision::Route {
                    format,
                    dispatch,
                    dialect,
                }) => {
                    return RouteOutcome::Routed(FileRoute {
                        rule,
                        format,
                        dispatch,
                        dialect,
                        size_cap: self.size_cap,
                    })
                }
                None => {}
            }
        }
        RouteOutcome::Unrouted
    }

    /// The route [`Self::route`] gives `rel_path` when the two template rules
    /// are skipped, or `None` when no other rule routes it.
    ///
    /// `file_category::Language` and `FileMeta` read this once per resolver
    /// candidate. The template rules scan every path component, and the
    /// paths those readers see are graph files, which the template rules
    /// already kept out of the index. So this reads the basename from the
    /// end of the path, and scans the components only for a YAML file.
    pub fn route_ignoring_templates(&self, rel_path: &str) -> Option<FileRoute> {
        let basename = rel_path
            .rsplit(['/', '\\'])
            .find(|component| !component.is_empty() && *component != ".")
            .unwrap_or("");
        let parts = PathParts {
            path: rel_path,
            basename,
            extension: extension_of(basename),
        };
        for &rule in PathRule::ALL {
            if matches!(
                rule,
                PathRule::TemplateDir | PathRule::TemplateInterpolation
            ) {
                continue;
            }
            if let Some(Decision::Route {
                format,
                dispatch,
                dialect,
            }) = rule.decide(&parts)
            {
                return Some(FileRoute {
                    rule,
                    format,
                    dispatch,
                    dialect,
                    size_cap: self.size_cap,
                });
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Exclude,
    Route {
        format: FileFormat,
        dispatch: Dispatch,
        dialect: Dialect,
    },
}

impl Decision {
    const fn fixed(format: FileFormat, language: Language) -> Self {
        Self::Route {
            format,
            dispatch: Dispatch::Fixed(language),
            dialect: Dialect::Standard,
        }
    }

    const fn sniffed(format: FileFormat, candidates: &'static [Language]) -> Self {
        Self::Route {
            format,
            dispatch: Dispatch::Sniffed(candidates),
            dialect: Dialect::Standard,
        }
    }
}

struct PathParts<'a> {
    path: &'a str,
    basename: &'a str,
    extension: Option<&'a str>,
}

impl<'a> PathParts<'a> {
    fn new(path: &'a str) -> Self {
        let basename = components(path).last().unwrap_or("");
        Self {
            path,
            basename,
            extension: extension_of(basename),
        }
    }

    fn has_yaml_extension(&self) -> bool {
        matches!(self.extension, Some("yml" | "yaml"))
    }
}

impl PathRule {
    fn decide(self, parts: &PathParts<'_>) -> Option<Decision> {
        match self {
            Self::TemplateDir => components(parts.path)
                .any(|component| component == "__brick__")
                .then_some(Decision::Exclude),
            Self::TemplateInterpolation => components(parts.path)
                .any(|component| component.contains("{{") || component.contains("}}"))
                .then_some(Decision::Exclude),
            Self::DockerfileName => matches!(parts.basename, "Dockerfile" | "dockerfile")
                .then_some(Decision::fixed(FileFormat::Code, Language::Dockerfile)),
            Self::ComposeName => matches!(
                parts.basename,
                "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml"
            )
            .then_some(Decision::fixed(FileFormat::Yaml, Language::DockerCompose)),
            Self::GitHubWorkflow => (parts.has_yaml_extension() && in_github_workflows(parts.path))
                .then_some(Decision::fixed(FileFormat::Yaml, Language::GitHubActions)),
            Self::YamlContent => parts
                .has_yaml_extension()
                .then_some(Decision::sniffed(FileFormat::Yaml, YAML_CANDIDATES)),
            Self::JsonContent => (parts.extension == Some("json"))
                .then_some(Decision::sniffed(FileFormat::Json, JSON_CANDIDATES)),
            Self::Extension => {
                let ext = parts.extension?;
                let language = Language::from_extension(ext)?;
                Some(Decision::Route {
                    format: FileFormat::Code,
                    dispatch: Dispatch::Fixed(language),
                    dialect: if ext == "tsx" {
                        Dialect::Tsx
                    } else {
                        Dialect::Standard
                    },
                })
            }
        }
    }
}

fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split(['/', '\\'])
        .filter(|component| !component.is_empty() && *component != ".")
}

/// `std::path::Path::extension` semantics on a basename: `None` without a
/// dot, and `None` for a dot-file such as `.bashrc`.
fn extension_of(basename: &str) -> Option<&str> {
    let (stem, ext) = basename.rsplit_once('.')?;
    (!stem.is_empty()).then_some(ext)
}

fn in_github_workflows(path: &str) -> bool {
    let mut previous = "";
    for component in components(path) {
        if previous == ".github" && component == "workflows" {
            return true;
        }
        previous = component;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> FileRouter {
        FileRouter::new(SizeCap::BUILT_IN)
    }

    fn routed(path: &str) -> FileRoute {
        match router().route(path) {
            RouteOutcome::Routed(route) => route,
            other => panic!("{path} is not routed: {other:?}"),
        }
    }

    #[test]
    fn test_language_all_discriminants_are_dense_in_order() {
        for (index, language) in Language::ALL.iter().enumerate() {
            assert_eq!(*language as usize, index, "{language:?}");
        }
        assert_eq!(Language::ALL.len(), 36);
    }

    #[test]
    fn test_capability_all_fits_the_set_bits() {
        assert!(Capability::ALL.len() <= u16::BITS as usize);
        for (index, capability) in Capability::ALL.iter().enumerate() {
            assert_eq!(*capability as usize, index, "{capability:?}");
        }
    }

    #[test]
    fn test_from_provider_every_language_round_trips() {
        for &language in Language::ALL {
            assert_eq!(Language::from_provider(language.provider()), Some(language));
        }
        assert_eq!(Language::from_provider("markdown"), None);
        assert_eq!(Language::from_provider(""), None);
    }

    #[test]
    fn test_from_extension_every_default_round_trips() {
        for &language in Language::ALL {
            for ext in language.extensions() {
                assert_eq!(Language::from_extension(ext), Some(language), "{ext}");
            }
        }
        assert_eq!(Language::from_extension("md"), None);
        assert_eq!(Language::from_extension(""), None);
        assert_eq!(Language::from_extension("RS"), None);
    }

    #[test]
    fn test_extensions_no_extension_in_two_languages() {
        let mut seen: Vec<&str> = Vec::new();
        for &language in Language::ALL {
            for ext in language.extensions() {
                assert!(!seen.contains(ext), "{ext} is listed twice");
                seen.push(ext);
            }
        }
    }

    #[test]
    fn test_extensions_rule_owned_extensions_are_not_defaults() {
        for ext in ["yml", "yaml", "json"] {
            assert_eq!(Language::from_extension(ext), None, "{ext}");
        }
    }

    #[test]
    fn test_route_every_language_reachable_by_some_rule() {
        let rule_paths = [
            "Dockerfile",
            "docker-compose.yml",
            ".github/workflows/ci.yml",
            "deploy.yaml",
            "spec.json",
        ];
        let mut reachable: Vec<Language> = rule_paths
            .iter()
            .flat_map(|path| routed(path).candidates().to_vec())
            .collect();
        for &language in Language::ALL {
            for ext in language.extensions() {
                reachable.extend(routed(&format!("src/x.{ext}")).candidates());
            }
        }
        for &language in Language::ALL {
            assert!(reachable.contains(&language), "{language:?} has no route");
        }
    }

    #[test]
    fn test_sniffed_candidates_hold_no_mainstream_language() {
        for candidates in [YAML_CANDIDATES, JSON_CANDIDATES] {
            assert!(!candidates.is_empty());
            for language in candidates {
                assert!(!Language::MAINSTREAM.contains(language), "{language:?}");
            }
        }
    }

    #[test]
    fn test_capabilities_mainstream_all_have_function_meta() {
        for language in Language::MAINSTREAM {
            assert!(
                language.capabilities().contains(Capability::FunctionMeta),
                "{language:?}"
            );
        }
    }

    #[test]
    fn test_display_name_matches_today_for_every_shared_variant() {
        for &language in Language::ALL {
            let legacy = language.to_legacy();
            if legacy != LegacyLanguage::Unknown {
                assert_eq!(language.display_name(), legacy.as_str(), "{language:?}");
            }
        }
    }

    #[test]
    fn test_capability_set_of_contains_only_listed() {
        let set = CapabilitySet::of(&[Capability::Flow, Capability::LexicalIndex]);
        assert!(set.contains(Capability::Flow));
        assert!(set.contains(Capability::LexicalIndex));
        assert!(!set.contains(Capability::PatternFinder));
        assert_eq!(
            set.iter().collect::<Vec<_>>(),
            vec![Capability::Flow, Capability::LexicalIndex]
        );
    }

    #[test]
    fn test_capability_set_empty_and_full_bounds() {
        assert!(CapabilitySet::EMPTY.is_empty());
        assert_eq!(CapabilitySet::default(), CapabilitySet::EMPTY);
        assert_eq!(CapabilitySet::FULL.iter().count(), Capability::ALL.len());
        let set = CapabilitySet::of(&[Capability::Flow]);
        assert_eq!(set.intersection(CapabilitySet::FULL), set);
        assert!(set.intersection(CapabilitySet::EMPTY).is_empty());
    }

    #[test]
    fn test_size_cap_resolve_env_wins() {
        let cap = SizeCap::resolve(Some("4096"), Some(2048));
        assert_eq!((cap.bytes(), cap.source()), (4096, SizeCapSource::Env));
    }

    #[test]
    fn test_size_cap_resolve_malformed_env_falls_to_config() {
        let cap = SizeCap::resolve(Some("4 MiB"), Some(2048));
        assert_eq!(
            (cap.bytes(), cap.source()),
            (2048, SizeCapSource::RepoConfig)
        );
        let cap = SizeCap::resolve(Some(""), Some(2048));
        assert_eq!(cap.source(), SizeCapSource::RepoConfig);
    }

    #[test]
    fn test_size_cap_resolve_nothing_set_built_in_one_mib() {
        let cap = SizeCap::resolve(None, None);
        assert_eq!(cap, SizeCap::BUILT_IN);
        assert_eq!(cap.bytes(), 1024 * 1024);
    }

    #[test]
    fn test_size_cap_admits_boundary_inclusive() {
        let cap = SizeCap::resolve(Some("10"), None);
        assert!(cap.admits(10));
        assert!(!cap.admits(11));
        assert!(cap.admits(0));
    }

    #[test]
    fn test_route_carries_router_size_cap() {
        let cap = SizeCap::resolve(None, Some(77));
        let route = FileRouter::new(cap).route("a.rs");
        assert_eq!(route.route().map(|r| r.size_cap()), Some(cap));
    }

    #[test]
    fn test_route_template_dir_excluded() {
        assert_eq!(
            router().route("packages/__brick__/lib/main.dart"),
            RouteOutcome::Excluded(PathRule::TemplateDir)
        );
    }

    #[test]
    fn test_route_template_interpolation_excluded() {
        assert_eq!(
            router().route("{{name.snakeCase()}}/widget.dart"),
            RouteOutcome::Excluded(PathRule::TemplateInterpolation)
        );
        assert_eq!(
            router().route("lib/a}}b.dart"),
            RouteOutcome::Excluded(PathRule::TemplateInterpolation)
        );
    }

    #[test]
    fn test_route_unknown_or_dotfile_unrouted() {
        for path in [
            "README.md",
            "notes.txt",
            "noext",
            ".bashrc",
            "src/",
            "",
            "a.RS",
        ] {
            assert_eq!(router().route(path), RouteOutcome::Unrouted, "{path:?}");
        }
    }

    #[test]
    fn test_route_rule_order_compose_name_beats_workflow_dir() {
        let route = routed(".github/workflows/compose.yml");
        assert_eq!(route.rule(), PathRule::ComposeName);
        assert_eq!(route.fixed_language(), Some(Language::DockerCompose));
    }

    #[test]
    fn test_route_workflow_any_depth_and_backslash_routes_github_actions() {
        for path in [
            ".github/workflows/ci.yml",
            "sub/.github/workflows/nested/ci.yaml",
            "sub\\.github\\workflows\\ci.yml",
        ] {
            let route = routed(path);
            assert_eq!(
                route.fixed_language(),
                Some(Language::GitHubActions),
                "{path}"
            );
            assert_eq!(route.format(), FileFormat::Yaml);
        }
        let route = routed(".github/ci.yml");
        assert_eq!(route.rule(), PathRule::YamlContent);
    }

    #[test]
    fn test_route_yaml_sniffed_in_gate_order() {
        let route = routed("k8s/deploy.yaml");
        assert_eq!(route.dispatch(), Dispatch::Sniffed(YAML_CANDIDATES));
        assert_eq!(route.fixed_language(), None);
        assert_eq!(
            route.providers().collect::<Vec<_>>(),
            vec!["kubernetes", "openapi"],
            "the yaml candidate builds no provider until C5 open point 6 lands"
        );
        assert!(route.capabilities_before_parse().is_empty());
    }

    #[test]
    fn test_route_json_sniffed_openapi_only() {
        let route = routed("api/spec.json");
        assert_eq!(route.format(), FileFormat::Json);
        assert_eq!(route.candidates(), &[Language::OpenApi]);
    }

    #[test]
    fn test_route_tsx_dialect_follows_extension() {
        let route = routed("ui/App.tsx");
        assert_eq!(route.dialect(), Dialect::Tsx);
        assert_eq!(routed("ui/app.ts").dialect(), Dialect::Standard);
    }

    #[test]
    fn test_route_h_header_routes_cpp() {
        assert_eq!(routed("include/a.h").fixed_language(), Some(Language::Cpp));
    }

    #[test]
    fn test_after_parse_fixed_route_file_equals_resolution() {
        let parsed = routed("src/lib.rs")
            .after_parse(Language::Rust, None)
            .unwrap();
        assert_eq!(
            (parsed.file(), parsed.resolution()),
            (Language::Rust, Language::Rust)
        );
    }

    #[test]
    fn test_after_parse_sniffed_accepts_any_candidate() {
        let route = routed("deploy.yaml");
        for &language in YAML_CANDIDATES {
            assert_eq!(route.after_parse(language, None).unwrap().file(), language);
        }
    }

    #[test]
    fn test_after_parse_non_candidate_rejected() {
        assert_eq!(
            routed("deploy.yaml").after_parse(Language::Rust, None),
            Err(AfterParseError::NotACandidate {
                accepted: Language::Rust
            })
        );
        assert_eq!(
            routed("src/lib.rs").after_parse(Language::Yaml, None),
            Err(AfterParseError::NotACandidate {
                accepted: Language::Yaml
            })
        );
    }

    #[test]
    fn test_after_parse_sfc_script_sets_resolution() {
        let parsed = routed("App.vue")
            .after_parse(Language::Vue, Some(Language::TypeScript))
            .unwrap();
        assert_eq!(parsed.file(), Language::Vue);
        assert_eq!(parsed.resolution(), Language::TypeScript);
    }

    #[test]
    fn test_after_parse_script_in_non_sfc_rejected() {
        assert_eq!(
            routed("src/lib.rs").after_parse(Language::Rust, Some(Language::TypeScript)),
            Err(AfterParseError::ScriptNotEmbeddable {
                file: Language::Rust,
                script: Language::TypeScript
            })
        );
        assert_eq!(
            routed("App.svelte").after_parse(Language::Svelte, Some(Language::Python)),
            Err(AfterParseError::ScriptNotEmbeddable {
                file: Language::Svelte,
                script: Language::Python
            })
        );
    }

    #[test]
    fn test_admits_global_same_resolution_only() {
        let ts = routed("a.ts")
            .after_parse(Language::TypeScript, None)
            .unwrap();
        let js = routed("a.js")
            .after_parse(Language::JavaScript, None)
            .unwrap();
        let vue_ts = routed("A.vue")
            .after_parse(Language::Vue, Some(Language::TypeScript))
            .unwrap();
        let vue_bare = routed("B.vue").after_parse(Language::Vue, None).unwrap();
        assert!(vue_ts.admits_global(ts));
        assert!(ts.admits_global(vue_ts));
        assert!(!ts.admits_global(js));
        assert!(!vue_bare.admits_global(ts));
        assert!(vue_bare.admits_global(vue_bare));
    }

    #[test]
    fn test_route_key_workflow_and_plain_yaml_differ() {
        assert_ne!(
            routed("deploy.yaml").key(),
            routed(".github/workflows/deploy.yaml").key()
        );
        assert_ne!(routed("a.yaml").key(), routed("a.json").key());
    }

    #[test]
    fn test_route_key_tsx_and_ts_differ() {
        assert_ne!(routed("a.ts").key(), routed("a.tsx").key());
    }

    #[test]
    fn test_route_key_same_route_any_directory_equal() {
        assert_eq!(routed("a.rs").key(), routed("b/c.rs").key());
        assert_eq!(routed("x.yml").key(), routed("k8s/y.yaml").key());
    }

    #[test]
    fn test_route_key_bytes_name_rule_dialect_language() {
        assert_eq!(
            routed("ui/App.tsx").key().to_bytes(),
            [
                PathRule::Extension as u8,
                Dialect::Tsx as u8,
                Language::TypeScript as u8
            ]
        );
        assert_eq!(routed("a.json").key().to_bytes()[2], RouteKey::SNIFFED);
    }

    #[test]
    fn test_route_key_sniffed_byte_names_no_language() {
        assert!(Language::ALL.len() < RouteKey::SNIFFED as usize);
    }

    #[test]
    fn test_size_cap_same_bytes_different_source_same_bytes() {
        let env = SizeCap::resolve(Some("4096"), None);
        let config = SizeCap::resolve(None, Some(4096));
        assert_ne!(env, config);
        assert_eq!(env.bytes(), config.bytes());
    }

    // C2 config file names (`contracts::workspace::Ecosystem::classify_config`)
    // that C3 classifies. Source and config are a set: a routed config file
    // is re-parsed and also invalidates its scopes.
    #[test]
    fn test_route_config_file_names_source_or_not() {
        for path in [
            "webpack.config.ts",
            "webpack.config.js",
            "jest.config.js",
            "jest.config.ts",
            "pkg/tsconfig.json",
            "package.json",
            "jsconfig.json",
            "jest.config.json",
        ] {
            assert!(router().route(path).route().is_some(), "{path} is source");
        }
        for path in [
            "Cargo.toml",
            "go.mod",
            "go.work",
            "pyproject.toml",
            "app/App.csproj",
        ] {
            assert_eq!(router().route(path), RouteOutcome::Unrouted, "{path}");
        }
    }

    #[test]
    fn test_route_outcome_route_accessor() {
        assert!(router().route("a.go").route().is_some());
        assert!(router().route("a.md").route().is_none());
        assert!(router().route("__brick__/a.go").route().is_none());
    }

    #[test]
    fn test_route_every_language_extension_first_provider_is_its_language() {
        for &language in Language::ALL {
            for ext in language.extensions() {
                let path = format!("src/x.{ext}");
                assert_eq!(
                    router()
                        .route(&path)
                        .route()
                        .and_then(|route| route.providers().next()),
                    Some(language.provider()),
                    "{path}"
                );
            }
        }
    }
}
