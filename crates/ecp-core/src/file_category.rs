//! Path-based file classification shared by the analyzer (persisted as
//! `File.category`), the resolver's candidate filter, impact's test filter
//! and the session overlay. One classifier, so a path that `impact` hides as
//! a test is also the path the resolver treats as a test candidate.

use crate::analyzer::types::RawImport;
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
            // `examples/tutorial/`). Test outranks Example: a test inside an
            // example app (`examples/todo/tests/`) is still a test, and its
            // fixtures (`@app.route('/test_setup')`) would pollute the
            // production-route surface.
            (PathPatternKind::Example, "/examples/"),
            (PathPatternKind::Example, "/example/"),
            (PathPatternKind::Example, "/sample/"),
            (PathPatternKind::Example, "/samples/"),
            (PathPatternKind::Example, "/demo/"),
            (PathPatternKind::Example, "/demos/"),
            // Test — substring forms. Basename suffix forms (`_test.go`,
            // `FooTest.java`) are handled separately: they depend on the
            // extension, so a bare `_spec.` substring would also catch
            // production files such as `lang_spec.rs`.
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
            // .NET test projects: `Foo.Tests/`, `Foo.UnitTests/` lowercase
            // to `.tests/` / `tests/`; the latter is caught by `/tests/`
            // only when it is a whole segment.
            (PathPatternKind::Test, ".tests/"),
            (PathPatternKind::Test, "/testdata/"),
            (PathPatternKind::Test, "/testutil"),
            (PathPatternKind::Test, "/test-utils/"),
            (PathPatternKind::Test, "/test_utils/"),
            (PathPatternKind::Test, "/test-helpers/"),
            (PathPatternKind::Test, "/test_helpers/"),
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
    // One allocation: `/` prefix so patterns like "/vendor/" also match a
    // top-level segment, `\\` folded to `/`, ASCII-lowercased (every
    // pattern is ASCII, so Unicode case folding cannot change a match).
    let mut lower_path = String::with_capacity(path.len() + 1);
    lower_path.push('/');
    lower_path.extend(path.chars().map(|c| match c {
        '\\' => '/',
        c => c.to_ascii_lowercase(),
    }));

    let (ac, kinds) = path_pattern_ac();
    let mut hit_example = false;
    let mut hit_test_substring = false;
    for m in ac.find_iter(&lower_path) {
        match kinds[m.pattern().as_usize()] {
            // Reference outranks Example and Test (vendored sample dirs
            // still classify as Reference); no later match can override.
            PathPatternKind::Reference => return FileCategory::Reference,
            PathPatternKind::Example => hit_example = true,
            PathPatternKind::Test => hit_test_substring = true,
        }
    }

    let is_test = hit_test_substring
        || has_test_suffix(&lower_path)
        // PascalCase test-class suffixes (Java/JUnit, Kotlin, Swift XCTest,
        // .NET MSTest/xUnit/NUnit, PHPUnit, ScalaTest/specs2). Case-sensitive
        // intentionally: `Manifest.java` lowercased ends with `test.java`, so
        // a case-insensitive check would mis-classify it. PascalCase `Test`
        // (capital T) is the language-mandated convention for these
        // ecosystems, so a literal `Test.ext` / `Tests.ext` / `Spec.ext`
        // suffix is a reliable signal.
        || path.ends_with("Test.java")
        || path.ends_with("Tests.java")
        || path.ends_with("Test.kt")
        || path.ends_with("Tests.kt")
        || path.ends_with("Tests.swift")
        || path.ends_with("Tests.cs")
        || path.ends_with("Test.cs")
        || path.ends_with("Test.php")
        || path.ends_with("Spec.scala")
        || path.ends_with("Test.scala");
    if is_test {
        return FileCategory::Test;
    }
    if hit_example {
        return FileCategory::Example;
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
    /// the caller is not a test file. That one is the target, once the
    /// caller supplies binding evidence ([`imports_reach`]).
    NonTest(u32),
    /// Several candidates remain, so the edge is suppressed.
    Ambiguous,
}

impl GlobalPick {
    pub fn target(self) -> Option<u32> {
        match self {
            Self::Unique(id) | Self::NonTest(id) => Some(id),
            Self::NoMatch | Self::Ambiguous => None,
        }
    }
}

/// The one Tier-3 candidate filter. The index-time resolver and the session
/// overlay both call it, so a barrier added here applies to both and the two
/// cannot disagree on a dirty file. `candidates` arrive kind-filtered.
///
/// The test tie-break runs only when the hard barriers leave two or more
/// candidates, and never changes a unique result. It is a preference, not a
/// barrier: a production caller whose only candidate sits in a test path
/// still resolves to it. On its own it is not evidence enough to emit an
/// edge (a fake of `run` makes `asyncio.run` look like the production
/// `run`), so callers gate `NonTest` on [`imports_reach`].
///
/// Returns as soon as the answer can only be `Ambiguous`: common names
/// carry thousands of candidates, and the rest of the list cannot change it.
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
        if count >= 2 && (caller.is_test || non_test >= 2) {
            return GlobalPick::Ambiguous;
        }
    }
    match count {
        0 => GlobalPick::NoMatch,
        1 => GlobalPick::Unique(first),
        _ if non_test == 1 => GlobalPick::NonTest(first_non_test),
        _ => GlobalPick::Ambiguous,
    }
}

/// Binding evidence for the test-double tie-break: one of the caller's
/// imports names the module that defines the candidate.
///
/// Import specifiers and paths are compared as lowercase segment lists, so
/// one rule covers Python `a.b.service`, TS `../lib/store`, Java
/// `com.x.Store`, PHP `App\\Models\\User`, Rust `crate::store`, C
/// `"store.h"`, Go `example.com/app/pkg/store` (a package is a directory),
/// C# `App.Services` and Swift `import Store` (both directories).
pub fn imports_reach(imports: &[RawImport], candidate_path: &str) -> bool {
    let (dir, module) = module_segments(candidate_path);
    imports.iter().any(|imp| {
        let source = if imp.source.is_empty() {
            imp.imported_name.as_str()
        } else {
            imp.source.as_str()
        };
        let spec = specifier_segments(source);
        if spec.is_empty() {
            return false;
        }
        // `from pkg import module` names the module in the imported name.
        let with_name = || {
            let mut v = spec.clone();
            v.extend(specifier_segments(&imp.imported_name));
            v
        };
        ends_with(&module, &spec)
            || (spec.len() >= 2 && ends_with(&module, &spec[..spec.len() - 1]))
            || (!dir.is_empty() && (ends_with(&spec, &dir) || ends_with(&dir, &spec)))
            || ends_with(&module, &with_name())
    })
}

const MODULE_EXTS: &[&str] = &[
    "h", "hh", "hpp", "hxx", "c", "cc", "cpp", "cxx", "py", "pyi", "ts", "tsx", "js", "jsx", "mjs",
    "cjs", "rb", "php", "go", "rs", "dart", "swift", "kt", "java", "cs",
];

/// `(directory segments, module segments)` of a repo-relative path, with the
/// extension and a package-entry basename (`index`, `__init__`, `mod`)
/// removed from the module.
fn module_segments(path: &str) -> (Vec<String>, Vec<String>) {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    let mut segs: Vec<String> = lower
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let file = segs.pop().unwrap_or_default();
    let stem = file
        .rsplit_once('.')
        .map_or(file.as_str(), |(stem, _)| stem);
    let dir = segs.clone();
    if !matches!(stem, "index" | "__init__" | "mod") {
        segs.extend(stem.split('.').filter(|s| !s.is_empty()).map(String::from));
    }
    (dir, segs)
}

fn specifier_segments(spec: &str) -> Vec<String> {
    let mut s = spec
        .trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>'))
        .to_ascii_lowercase();
    if let Some(rest) = s.strip_prefix("package:") {
        // Dart `package:<name>/path` maps to the package's `lib/`.
        s = rest.split_once('/').map_or("", |(_, p)| p).to_string();
    }
    if let Some((stem, ext)) = s.rsplit_once('.') {
        if MODULE_EXTS.contains(&ext) && (s.contains('/') || !stem.contains('.')) {
            s.truncate(stem.len());
        }
    }
    s.split(|c| matches!(c, '/' | '.' | ':' | '\\'))
        .filter(|seg| !matches!(*seg, "" | "crate" | "self" | "super"))
        .map(String::from)
        .collect()
}

fn ends_with(hay: &[String], tail: &[String]) -> bool {
    !tail.is_empty() && hay.len() >= tail.len() && hay[hay.len() - tail.len()..] == *tail
}

/// Test files named by their basename.
///
/// - Sibling test modules: Rust `tests.rs`, Django `tests.py`, `test.js`.
///   Doc and config files are excluded, so a CI workflow `test.yml` stays
///   Config.
/// - Co-located suffixes, limited to the ecosystems whose runners use them:
///   Go `_test.go`, pytest `_test.py`, Minitest `_test.rb`, gtest
///   `_test.cc`, Jest/Jasmine `_test.ts` / `_spec.ts`, RSpec `_spec.rb`,
///   Crystal `_spec.cr`. Rust, Dart and Move keep tests under `tests/` /
///   `test/`, so `lang_spec.rs` or a package named `bloc_test` stays
///   production code.
fn has_test_suffix(lower_path: &str) -> bool {
    let base = lower_path.rsplit('/').next().unwrap_or("");
    let Some((stem, ext)) = base.rsplit_once('.') else {
        return false;
    };
    const JS: &[&str] = &["js", "jsx", "ts", "tsx", "mjs", "cjs"];
    (matches!(stem, "test" | "tests")
        && !matches!(
            ext,
            "md" | "txt" | "rst" | "json" | "toml" | "yaml" | "yml" | "html" | "css"
        ))
        || (stem.ends_with("_test")
            && (JS.contains(&ext)
                || matches!(ext, "go" | "py" | "rb" | "c" | "cc" | "cpp" | "cxx" | "exs")))
        || (stem.ends_with("_spec") && (JS.contains(&ext) || matches!(ext, "rb" | "cr")))
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
    fn test_determine_category_per_language_conventions_classify_both_ways() {
        // One test-file and one production-file convention per mainstream
        // language, so narrowing a rule for one ecosystem shows up red.
        let cases: &[(&str, &str, &str)] = &[
            ("ts", "src/app/util.test.ts", "src/app/util.ts"),
            ("js", "src/app/util_spec.js", "src/app/util.js"),
            ("py", "pkg/app/tests.py", "pkg/app/models.py"),
            (
                "java",
                "src/main/java/com/x/FooTest.java",
                "src/main/java/com/x/Foo.java",
            ),
            ("kt", "app/src/FooTests.kt", "app/src/Foo.kt"),
            ("cs", "src/Foo.Tests/TestObjects/Bar.cs", "src/Foo/Bar.cs"),
            ("go", "pkg/store/store_test.go", "pkg/store/store.go"),
            (
                "rs",
                "crates/x/src/flow/tests.rs",
                "crates/x/src/lang_spec.rs",
            ),
            ("php", "app/Models/UserTest.php", "app/Models/User.php"),
            ("rb", "lib/user_spec.rb", "lib/user.rb"),
            (
                "swift",
                "Sources/AppTests/LoginTests.swift",
                "Sources/App/Login.swift",
            ),
            ("c", "src/net/socket_test.c", "src/net/socket.c"),
            ("cpp", "src/net/socket_test.cc", "src/net/socket.cc"),
            ("dart", "test/parser_test.dart", "lib/bloc_test.dart"),
        ];
        for (lang, test_path, prod_path) in cases {
            assert_eq!(
                determine_category(test_path),
                FileCategory::Test,
                "{lang}: expected Test for {test_path}"
            );
            assert_eq!(
                determine_category(prod_path),
                FileCategory::Source,
                "{lang}: expected Source for {prod_path}"
            );
        }
    }

    #[test]
    fn test_determine_category_shared_test_support_dirs_classify_as_test() {
        for p in [
            "internal/testutil/fake.go",
            "pkg/parser/testdata/input.go",
            "src/test-helpers/render.ts",
            "src/test_utils/factory.py",
        ] {
            assert_test(p);
        }
    }

    #[test]
    fn test_determine_category_test_inside_example_app_classifies_as_test() {
        // Test outranks Example: a nested test must not emit fixture routes.
        assert_test("examples/todo/tests/test_routes.py");
        assert_test("test/node/fixtures/examples/a.js");
        assert_example("examples/todo/app.py");
    }

    #[test]
    fn test_determine_category_test_named_config_stays_config() {
        assert_eq!(
            determine_category(".github/workflows/test.yml"),
            FileCategory::Config
        );
        assert_eq!(determine_category("docs/tests.md"), FileCategory::Document);
    }

    #[test]
    fn test_determine_category_windows_separators_classify_like_unix() {
        assert_test("src\\Foo.Tests\\Bar.cs");
        assert_test("pkg\\store\\store_test.go");
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
        assert_eq!(pick, GlobalPick::Ambiguous);
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
        assert_eq!(pick, GlobalPick::Ambiguous);
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

    use super::imports_reach;
    use crate::analyzer::types::RawImport;

    fn imp(source: &str, name: &str) -> RawImport {
        RawImport {
            source: source.to_string(),
            imported_name: name.to_string(),
            alias: None,
            binding_kind: None,
        }
    }

    #[test]
    fn test_imports_reach_each_language_import_shape_reaches_its_module() {
        // Specifier shapes as the 14 mainstream parsers emit them.
        let cases: &[(&str, RawImport, &str)] = &[
            (
                "py",
                imp("enoract.shared.client.google.service", "FlightService"),
                "enoract/shared/client/google/service.py",
            ),
            ("py-module", imp("", "os.path"), "os/path.py"),
            ("ts", imp("../lib/store", "scan"), "lib/store.ts"),
            ("js", imp("./lib", "scan"), "src/lib/index.js"),
            (
                "java",
                imp("com.app.store.Store", "Store"),
                "src/main/java/com/app/store/Store.java",
            ),
            (
                "kt",
                imp("com.app.store.Store", "com.app.store.Store"),
                "app/src/main/kotlin/com/app/store/Store.kt",
            ),
            (
                "cs",
                imp("App.Services", "App.Services"),
                "src/App/Services/FlightService.cs",
            ),
            (
                "go",
                imp("example.com/app/pkg/store", "store"),
                "pkg/store/scan.go",
            ),
            ("rs", imp("crate::store", "scan"), "src/store/mod.rs"),
            (
                "php",
                imp("App\\Models\\User", "User"),
                "app/Models/User.php",
            ),
            ("rb", imp("../lib/store", "../lib/store"), "lib/store.rb"),
            (
                "swift",
                imp("Store", "Store"),
                "Sources/Store/Scanner.swift",
            ),
            ("c", imp("\"store.h\"", "*"), "src/store.c"),
            (
                "cpp",
                imp("net/store.hpp", "net/store.hpp"),
                "src/net/store.cpp",
            ),
            (
                "dart",
                imp("package:app/store.dart", "package:app/store.dart"),
                "lib/store.dart",
            ),
        ];
        for (lang, import, path) in cases {
            assert!(
                imports_reach(std::slice::from_ref(import), path),
                "{lang}: {:?} should reach {path}",
                import.source
            );
        }
    }

    #[test]
    fn test_imports_reach_unrelated_imports_do_not_reach() {
        // `asyncio.run` in a CLI must not reach a project `cloudflare.run`.
        let imports = [
            imp("asyncio", "asyncio"),
            imp("typing", "Any"),
            imp("<stdio.h>", "*"),
        ];
        assert!(!imports_reach(
            &imports,
            "enoract/shared/client/cloudflare.py"
        ));
        assert!(!imports_reach(&imports, "src/stdio_utils.c"));
        assert!(!imports_reach(&[], "src/store.py"));
    }
}
