//! Path-based file classification shared by the analyzer (persisted as
//! `File.category`), the resolver's candidate filter, impact's test filter
//! and the session overlay. One classifier, so a path that `impact` hides as
//! a test is also the path the resolver treats as a test candidate.

use crate::graph::FileCategory;
use aho_corasick::{AhoCorasick, MatchKind};
use std::sync::OnceLock;

#[derive(Copy, Clone)]
enum PathPatternKind {
    Reference,
    Example,
    Test,
}

/// Substring patterns used by `determine_category`, scanned in one
/// Aho-Corasick pass instead of N independent `contains()` calls. The
/// 25k-file cold index used to spend ~150k substring scans here
/// (35 patterns × 4286 files-per-cpu-core); a single AC pass collapses
/// that to one scan per file with constant-time per-character work,
/// surfaced by the PR #149 simplify review.
///
/// The PascalCase test suffixes (`Test.java`, `Tests.kt`, `Spec.scala`)
/// stay on `ends_with` — they're suffix-anchored and need case-sensitive
/// comparison against the original-cased path (`Manifest.java`
/// lowercased ends with `test.java`, so a case-insensitive scan would
/// mis-classify it).
static PATH_PATTERN_AC: OnceLock<(AhoCorasick, Vec<PathPatternKind>)> = OnceLock::new();

fn path_pattern_ac() -> &'static (AhoCorasick, Vec<PathPatternKind>) {
    PATH_PATTERN_AC.get_or_init(|| {
        // (kind, pattern) — kept verbatim from the original cascade so
        // the diff against the pre-AC implementation stays mechanical.
        const PATTERNS: &[(PathPatternKind, &str)] = &[
            // Reference — vendored / installed deps, never user-authored source.
            (PathPatternKind::Reference, "/vendor/"),
            (PathPatternKind::Reference, "/node_modules/"),
            (PathPatternKind::Reference, "/.venv/"),
            (PathPatternKind::Reference, "/venv/"),
            (PathPatternKind::Reference, "/site-packages/"),
            (PathPatternKind::Reference, "/.tox/"),
            (PathPatternKind::Reference, "/.bundle/"),
            (PathPatternKind::Reference, "/gems/"),
            (PathPatternKind::Reference, "/.pub-cache/"),
            (PathPatternKind::Reference, "/.gradle/"),
            (PathPatternKind::Reference, "/.m2/"),
            (PathPatternKind::Reference, "/pods/"),
            (PathPatternKind::Reference, "/carthage/"),
            (PathPatternKind::Reference, "/.build/"),
            (PathPatternKind::Reference, "/third_party/"),
            (PathPatternKind::Reference, "/external/"),
            (PathPatternKind::Reference, "/deps/"),
            // Example / sample / demo — canonical "how to use this framework"
            // content. Surfaced separately from Test so routes / tools /
            // handlers under `/examples/` stay visible to LLM consumers
            // (Express's `examples/auth/`, NestJS `sample/`, Flask
            // `examples/tutorial/`). `/tests/` stays as Test because test
            // fixtures (`@app.route('/test_setup')`, helper test endpoints)
            // would pollute the production-route surface.
            (PathPatternKind::Example, "/examples/"),
            (PathPatternKind::Example, "/example/"),
            (PathPatternKind::Example, "/sample/"),
            (PathPatternKind::Example, "/samples/"),
            (PathPatternKind::Example, "/demo/"),
            (PathPatternKind::Example, "/demos/"),
            // Test — substring forms. Suffix forms (`_test.go`, etc.) are
            // handled separately because `ends_with` already runs in
            // constant time against the path tail.
            (PathPatternKind::Test, ".test."),
            (PathPatternKind::Test, ".spec."),
            // NestJS / Angular `.e2e-spec.ts` etc.
            (PathPatternKind::Test, "-spec."),
            (PathPatternKind::Test, "__tests__/"),
            (PathPatternKind::Test, "__mocks__/"),
            (PathPatternKind::Test, "/test/"),
            (PathPatternKind::Test, "/tests/"),
            (PathPatternKind::Test, "/testing/"),
            (PathPatternKind::Test, "/fixtures/"),
            // Cypress / NestJS / Playwright e2e dirs.
            (PathPatternKind::Test, "/e2e/"),
            (PathPatternKind::Test, "/spec/"),
            (PathPatternKind::Test, "/test_"),
            (PathPatternKind::Test, "/conftest."),
            // Co-located suffix forms in any language: Go `_test.go`, C++
            // gtest `_test.cc`, Dart `_test.dart`, RSpec / Jasmine `_spec.`.
            (PathPatternKind::Test, "_test."),
            (PathPatternKind::Test, "_spec."),
        ];
        let strings: Vec<&str> = PATTERNS.iter().map(|(_, s)| *s).collect();
        let kinds: Vec<PathPatternKind> = PATTERNS.iter().map(|(k, _)| *k).collect();
        let ac = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(strings)
            .expect("path-pattern AC build");
        (ac, kinds)
    })
}

pub fn determine_category(path: &str) -> FileCategory {
    let normalized_path = path.replace('\\', "/");
    // Prefix with "/" so patterns like "/vendor/" match both embedded
    // segments AND top-level paths (e.g. `vendor/foo` → `/vendor/foo`).
    let lower_path = format!("/{}", normalized_path.to_lowercase());

    let (ac, kinds) = path_pattern_ac();
    let mut hit_reference = false;
    let mut hit_example = false;
    let mut hit_test_substring = false;
    for m in ac.find_iter(&lower_path) {
        match kinds[m.pattern().as_usize()] {
            PathPatternKind::Reference => {
                // Reference outranks Example and Test (vendored sample
                // dirs still classify as Reference). Bail early — no
                // later match can override.
                hit_reference = true;
                break;
            }
            PathPatternKind::Example => hit_example = true,
            PathPatternKind::Test => hit_test_substring = true,
        }
    }
    if hit_reference {
        return FileCategory::Reference;
    }
    if hit_example {
        return FileCategory::Example;
    }

    let is_test = hit_test_substring
        // PascalCase test-class suffixes (Java/JUnit, Kotlin, Swift XCTest,
        // .NET MSTest/xUnit/NUnit, PHPUnit, ScalaTest/specs2). Case-sensitive
        // intentionally: `Manifest.java` lowercased ends with `test.java`, so
        // a case-insensitive check would mis-classify it. PascalCase `Test`
        // (capital T) is the language-mandated convention for these
        // ecosystems, so a literal `Test.ext` / `Tests.ext` / `Spec.ext`
        // suffix is a reliable signal.
        || normalized_path.ends_with("Test.java")
        || normalized_path.ends_with("Tests.java")
        || normalized_path.ends_with("Test.kt")
        || normalized_path.ends_with("Tests.kt")
        || normalized_path.ends_with("Tests.swift")
        || normalized_path.ends_with("Tests.cs")
        || normalized_path.ends_with("Test.cs")
        || normalized_path.ends_with("Test.php")
        || normalized_path.ends_with("Spec.scala")
        || normalized_path.ends_with("Test.scala");
    if is_test {
        return FileCategory::Test;
    }

    if lower_path.ends_with(".md") || lower_path.ends_with(".txt") || lower_path.ends_with(".rst") {
        return FileCategory::Document;
    }
    if lower_path.ends_with(".json")
        || lower_path.ends_with(".toml")
        || lower_path.ends_with(".yaml")
        || lower_path.ends_with(".yml")
        || lower_path.ends_with("dockerfile")
    {
        return FileCategory::Config;
    }
    FileCategory::Source
}

/// Per-parser-provider language tag. One variant per registered analyzer
/// provider; `from_path` performs the lookup by file extension (multi-ext
/// providers like JavaScript / TypeScript fold to a single variant).
///
/// Used by [`pick_global`] as a Tier-3 caller-vs-target barrier:
/// bare callee names never cross language boundaries (a Rust `result.is_some()`
/// never resolves to a vendored Move test fixture's `is_some` function).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    #[default]
    Unknown,
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Java,
    Kotlin,
    Go,
    Ruby,
    Php,
    CSharp,
    Swift,
    Dart,
    Solidity,
    Sql,
    C,
    Cpp,
    Move,
    Nim,
    Cairo,
    Vyper,
    Verilog,
    Hcl,
    Crystal,
    Lua,
    Zig,
    Bash,
    Dockerfile,
    DockerCompose,
    GitHubActions,
    Yaml,
    Markdown,
    // Appended at the end to keep variant order stable (defensive — `Language`
    // is not rkyv-archived today, but other resolution code matches on it).
    Kubernetes,
}

impl Language {
    /// Map a repo-relative file path to its provider language. Mirrors the
    /// extension routing in `commands/analyze.rs` plus path-based overrides
    /// for `Dockerfile` / `docker-compose.{yml,yaml}` / `.github/workflows/*`.
    pub fn from_path(path: &str) -> Self {
        if path.contains('\\') {
            let normalized = path.replace('\\', "/");
            Self::from_normalized_path(&normalized)
        } else {
            Self::from_normalized_path(path)
        }
    }

    /// Fast path for callers that have already converted backslashes (Pass 1
    /// in `builder.rs` and most repo-rooted paths on Linux/macOS). Skips the
    /// `replace('\\','/')` allocation entirely.
    pub fn from_normalized_path(path: &str) -> Self {
        let normalized = path;
        let basename = normalized.rsplit('/').next().unwrap_or("");

        // Path / basename overrides before extension routing.
        if matches!(basename, "Dockerfile" | "dockerfile") {
            return Self::Dockerfile;
        }
        if matches!(
            basename,
            "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml"
        ) {
            return Self::DockerCompose;
        }
        let ext = basename.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if matches!(ext, "yml" | "yaml")
            && (normalized.contains("/.github/workflows/")
                || normalized.starts_with(".github/workflows/"))
        {
            return Self::GitHubActions;
        }

        match ext {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "java" => Self::Java,
            "kt" | "kts" => Self::Kotlin,
            "go" => Self::Go,
            "rb" => Self::Ruby,
            "php" => Self::Php,
            "cs" => Self::CSharp,
            "swift" => Self::Swift,
            "dart" => Self::Dart,
            "sol" => Self::Solidity,
            "sql" => Self::Sql,
            "c" => Self::C,
            // `.h` routes to C++ (matches ref-gitnexus dispatch). `.h` is genuinely
            // ambiguous — C headers and C++ headers share the extension — but C++
            // parsing is a near-superset of C, while C parsing produces ERROR
            // nodes on any C++-only construct (class, template, namespace, &,
            // operator overload). Real codebases ship C++ libraries with `.h`
            // headers (nlohmann/json, doctest, LLVM Fuzzer, Catch2, …); routing
            // them to the C parser silently drops every class/method/template.
            "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" | "h" => Self::Cpp,
            "move" => Self::Move,
            "nim" => Self::Nim,
            "cairo" => Self::Cairo,
            "vy" => Self::Vyper,
            "v" | "sv" | "vh" | "svh" => Self::Verilog,
            "tf" | "tfvars" | "hcl" => Self::Hcl,
            "cr" => Self::Crystal,
            "lua" | "luau" => Self::Lua,
            "zig" => Self::Zig,
            "sh" | "bash" => Self::Bash,
            "yml" | "yaml" => Self::Yaml,
            "md" | "txt" | "rst" => Self::Markdown,
            _ => Self::Unknown,
        }
    }

    /// Canonical display name. Used by `ecp find --mode bm25` to emit the `language`
    /// field on each Hit. Overrides Debug formatting for `Php` ("PHP") and
    /// `Cpp` ("C++") where the conventional name differs from the variant
    /// identifier.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Rust => "Rust",
            Self::Python => "Python",
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Java => "Java",
            Self::Kotlin => "Kotlin",
            Self::Go => "Go",
            Self::Ruby => "Ruby",
            Self::Php => "PHP",
            Self::CSharp => "CSharp",
            Self::Swift => "Swift",
            Self::Dart => "Dart",
            Self::Solidity => "Solidity",
            Self::Sql => "SQL",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Move => "Move",
            Self::Nim => "Nim",
            Self::Cairo => "Cairo",
            Self::Vyper => "Vyper",
            Self::Verilog => "Verilog",
            Self::Hcl => "HCL",
            Self::Crystal => "Crystal",
            Self::Lua => "Lua",
            Self::Zig => "Zig",
            Self::Bash => "Bash",
            Self::Dockerfile => "Dockerfile",
            Self::DockerCompose => "DockerCompose",
            Self::GitHubActions => "GitHubActions",
            Self::Yaml => "YAML",
            Self::Markdown => "Markdown",
            Self::Kubernetes => "Kubernetes",
        }
    }
}

/// Per-file facts the Tier-3 candidate filter reads. Computed once per file
/// and cached per node id, so [`pick_global`] costs O(1) per candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileMeta {
    /// Path contains a `/vendor/` segment. Non-vendor callers must not resolve
    /// to vendor targets — vendor grammar test corpora share short common names
    /// (`is_some`, `get`, `new`) with stdlib methods, producing one false edge
    /// per call site that survives the kind+unique filter.
    pub is_vendor: bool,
    /// [`determine_category`] is `Test`. Production code cannot call a test
    /// double, so a non-test caller prefers the one non-test candidate.
    pub is_test: bool,
    pub language: Language,
}

impl FileMeta {
    pub fn from_path(path: &str) -> Self {
        if path.contains('\\') {
            let normalized = path.replace('\\', "/");
            Self::from_normalized_path(&normalized)
        } else {
            Self::from_normalized_path(path)
        }
    }

    /// Fast path for callers that have already normalised separators.
    pub fn from_normalized_path(path: &str) -> Self {
        Self::with_category(path, determine_category(path))
    }

    /// For callers that already hold the file's category: Pass 1 computes it
    /// once for `File.category`, and the overlay reads it back from the graph.
    pub fn with_category(path: &str, category: FileCategory) -> Self {
        Self {
            is_vendor: path.contains("/vendor/") || path.starts_with("vendor/"),
            is_test: category == FileCategory::Test,
            language: Language::from_normalized_path(path),
        }
    }

    /// Hard barriers: a candidate in another language, or a vendor candidate
    /// for a non-vendor caller, is never a resolution target.
    pub fn admits(self, candidate: FileMeta) -> bool {
        candidate.language == self.language && (!candidate.is_vendor || self.is_vendor)
    }
}

/// Outcome of the bare-name global (Tier 3) lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalPick {
    /// No candidate passed the barriers.
    NoMatch,
    /// Exactly one candidate passed the barriers.
    Unique(u32),
    /// Several candidates passed, only one of them outside test files, and
    /// the caller is not a test file. That one is the target.
    NonTest(u32),
    /// Several candidates remain, so the edge is suppressed. Carries the
    /// post-barrier candidate count.
    Ambiguous(u32),
}

impl GlobalPick {
    pub fn target(self) -> Option<u32> {
        match self {
            Self::Unique(id) | Self::NonTest(id) => Some(id),
            Self::NoMatch | Self::Ambiguous(_) => None,
        }
    }
}

/// The one Tier-3 candidate filter. The index-time resolver and the session
/// overlay both call it, so a barrier added here applies to both and the two
/// cannot disagree on a dirty file. `candidates` arrive kind-filtered.
///
/// The test tie-break runs only when the hard barriers leave two or more
/// candidates. It adds an edge where the unique-only rule would drop one and
/// never changes a unique result, so recall cannot fall. It is a preference,
/// not a barrier: a production caller whose only candidate sits in a test
/// path still resolves to it.
pub fn pick_global(
    caller: FileMeta,
    candidates: impl IntoIterator<Item = (u32, FileMeta)>,
) -> GlobalPick {
    let (mut count, mut first) = (0u32, 0u32);
    let (mut non_test, mut first_non_test) = (0u32, 0u32);
    for (id, meta) in candidates {
        if !caller.admits(meta) {
            continue;
        }
        if count == 0 {
            first = id;
        }
        count += 1;
        if !meta.is_test {
            if non_test == 0 {
                first_non_test = id;
            }
            non_test += 1;
        }
    }
    match count {
        0 => GlobalPick::NoMatch,
        1 => GlobalPick::Unique(first),
        _ if non_test == 1 && !caller.is_test => GlobalPick::NonTest(first_non_test),
        n => GlobalPick::Ambiguous(n),
    }
}

/// `true` when [`determine_category`] classifies `path` as `Test`. Every
/// test/non-test decision outside the persisted `File.category` goes through
/// here, so impact, process detection, function-meta flags and the resolver
/// agree with the graph.
pub fn is_test_path(path: &str) -> bool {
    determine_category(path) == FileCategory::Test
}

#[cfg(test)]
mod determine_category_tests {
    use super::{determine_category, is_test_path, FileCategory};

    fn assert_test(path: &str) {
        assert_eq!(
            determine_category(path),
            FileCategory::Test,
            "expected Test for {path}",
        );
    }

    fn assert_source(path: &str) {
        assert_eq!(
            determine_category(path),
            FileCategory::Source,
            "expected Source for {path}",
        );
    }

    fn assert_example(path: &str) {
        assert_eq!(
            determine_category(path),
            FileCategory::Example,
            "expected Example for {path}",
        );
    }

    #[test]
    fn java_kotlin_swift_csharp_php_scala_test_suffixes_classify_as_test() {
        // Per-language test-file conventions added in PR #51.
        assert_test("src/main/java/com/foo/BarTest.java"); // JUnit
        assert_test("src/main/java/com/foo/BarTests.java"); // JUnit alt
        assert_test("app/src/main/kotlin/FooTest.kt"); // Kotlin
        assert_test("app/src/main/kotlin/FooTests.kt"); // Kotlin alt
        assert_test("MyAppTests/AuthFlowTests.swift"); // Swift XCTest
        assert_test("src/Auth.Tests/LoginTests.cs"); // .NET xUnit
        assert_test("src/MyApp/UserTest.cs"); // .NET MSTest alt
        assert_test("app/Http/Controllers/UserControllerTest.php"); // PHPUnit
        assert_test("src/main/scala/com/foo/BarSpec.scala"); // ScalaTest
        assert_test("src/main/scala/com/foo/BarTest.scala"); // ScalaTest alt
    }

    #[test]
    fn example_sample_demo_dirs_classify_as_example() {
        // Round 80 split: framework example/sample/demo dirs are canonical
        // "how to wire routes" content that LLM consumers want to navigate
        // (Express's `examples/auth/`, NestJS's `sample/`, Flask's
        // `examples/tutorial/`). Previously these collapsed into `Test`,
        // which the builder skipped — ecp emitted zero Routes for the
        // 82-row JS examples corpus. Now they classify as `Example` and
        // routes flow through normally; `/tests/` / `.spec.` / Cypress
        // `/e2e/` stay as Test (test fixtures still must not pollute the
        // production-route surface).
        assert_example("JavaScript/examples/auth/index.js");
        assert_example("Ruby/examples/chat.rb");
        assert_example("Python/examples/flask_basic.py");
        assert_example("TypeScript/sample/01-cats-app/src/cats.controller.ts");
        assert_example("packages/demo/index.html");
        // E2E spec files still classify as Test — they're tests against
        // routes, not example apps demonstrating routes.
        assert_test("apps/foo/e2e/login.spec.ts");
        assert_test("apps/foo/src/auth.e2e-spec.ts");
        assert_test("frontend/cypress/e2e/login.cy.ts");
    }

    #[test]
    fn ambiguous_substrings_do_not_classify_as_test() {
        // Guard against the new patterns becoming false-positive magnets.
        // `sample`, `example`, `demo`, `e2e` are common nouns and the
        // path filter must require the literal `/<token>/` segment form,
        // not bare substring matches.
        assert_source("src/sampleRate.ts"); // var name, not /sample/
        assert_source("src/Examples.kt"); // exported public type
        assert_source("src/demographics/service.py");
        assert_source("src/e2encoder/utils.go"); // no /e2e/ segment
        assert_source("src/lib/spec_loader.py"); // _spec at start of basename
    }

    #[test]
    fn non_test_files_classify_as_source() {
        // Files whose names happen to contain the substring "test" but aren't
        // test files — these would mis-classify if the suffix check were
        // case-insensitive, since after lowercasing `Manifest.java` ends with
        // `test.java`. Case-sensitive PascalCase suffix matching keeps them
        // as Source.
        assert_source("src/main/java/com/foo/Manifest.java"); // ends with test.java when lowercased
        assert_source("src/main/java/com/foo/Tester.java");
        assert_source("src/main/java/com/foo/Contestant.java");
        assert_source("app/src/main/kotlin/StressTester.kt");
        assert_source("src/Trading/Backtest.cs"); // lowercased ends with `test.cs`
        assert_source("app/src/main/scala/com/foo/Latest.scala"); // lowercased ends with `test.scala`
                                                                  // Also confirm production code with PascalCase test-like names but
                                                                  // not the literal `Test.ext` suffix stays Source.
        assert_source("src/Auth/AttestationService.cs");
    }

    #[test]
    fn test_is_test_path_root_relative_test_dirs_classify_as_test() {
        // Graph paths are repo-relative, so a root `tests/` dir has no
        // leading `/`. pytest's `conftest.py` and shared fakes live there.
        for p in [
            "tests/conftest.py",
            "tests/_fakes.py",
            "test/helpers.js",
            "conftest.py",
            "test_app.py",
            "spec/models/user_spec.rb",
        ] {
            assert!(is_test_path(p), "expected test: {p}");
        }
    }

    #[test]
    fn test_determine_category_underscore_test_any_extension_classifies_as_test() {
        // Co-located `_test` / `_spec` suffixes beyond Go/Python/Ruby: C++
        // gtest `foo_test.cc`, Dart `foo_test.dart`, Jasmine `foo_spec.js`.
        assert_test("src/net/socket_test.cc");
        assert_test("lib/src/parser_test.dart");
        assert_test("src/app/util_spec.js");
        assert_test("src/app/util_test.ts");
    }

    #[test]
    fn test_is_test_path_test_prefixed_names_not_test() {
        // `/test` as a bare prefix used to match these production paths.
        for p in [
            "src/testimonials/service.py",
            "src/tester.py",
            "app/testbed_config.go",
        ] {
            assert!(!is_test_path(p), "expected non-test: {p}");
        }
    }

    use super::{pick_global, FileMeta, GlobalPick};

    fn meta(path: &str) -> FileMeta {
        FileMeta::from_path(path)
    }

    #[test]
    fn test_pick_global_production_caller_with_test_double_picks_production() {
        let caller = meta("src/app/search.py");
        let pick = pick_global(
            caller,
            [
                (1, meta("src/client/service.py")),
                (2, meta("tests/fakes.py")),
            ],
        );
        assert_eq!(pick, GlobalPick::NonTest(1));
    }

    #[test]
    fn test_pick_global_test_caller_with_test_double_stays_ambiguous() {
        // A test may call either the real definition or its fake.
        let caller = meta("tests/test_search.py");
        let pick = pick_global(
            caller,
            [
                (1, meta("src/client/service.py")),
                (2, meta("tests/fakes.py")),
            ],
        );
        assert_eq!(pick, GlobalPick::Ambiguous(2));
    }

    #[test]
    fn test_pick_global_two_production_candidates_stay_ambiguous() {
        let caller = meta("src/app/search.py");
        let pick = pick_global(
            caller,
            [
                (1, meta("src/a.py")),
                (2, meta("src/b.py")),
                (3, meta("tests/fakes.py")),
            ],
        );
        assert_eq!(pick, GlobalPick::Ambiguous(3));
    }

    #[test]
    fn test_pick_global_only_test_candidate_still_resolves() {
        // A tie-break, not a barrier: a production caller whose single
        // candidate sits in a test-classified path keeps the edge.
        let caller = meta("src/app/search.py");
        assert_eq!(
            pick_global(caller, [(7, meta("src/testing/helpers.py"))]),
            GlobalPick::Unique(7)
        );
    }

    #[test]
    fn test_pick_global_language_and_vendor_barriers_drop_candidates() {
        let caller = meta("src/main.rs");
        assert_eq!(
            pick_global(caller, [(1, meta("lib/option.move"))]),
            GlobalPick::NoMatch
        );
        assert_eq!(
            pick_global(caller, [(1, meta("crates/vendor/x/src/lib.rs"))]),
            GlobalPick::NoMatch
        );
        assert_eq!(
            pick_global(
                caller,
                [
                    (1, meta("crates/vendor/x/src/lib.rs")),
                    (2, meta("src/lib.rs"))
                ]
            ),
            GlobalPick::Unique(2)
        );
    }

    #[test]
    fn test_pick_global_empty_candidates_no_match() {
        assert_eq!(
            pick_global(meta("src/main.rs"), std::iter::empty()),
            GlobalPick::NoMatch
        );
    }
}
