//! `FunctionMeta::FLAG_TEST` needs evidence: a test file, or an attribute,
//! annotation or decorator a test framework reads (`#[test]`, `@Test`,
//! `[Fact]`, `@pytest.mark`, `#[Test]`). A test-like name alone in a
//! production file is no evidence: `testUrlMatchesGlob` in vscode's
//! `urlGlob.ts` is production code, and `ecp impact` reads the flag to tag
//! and hide test callers.
//!
//! Per language: a test-like name in a production file is not a test, and
//! the language's strongest evidence in that file (or a test file, where the
//! language has no attribute) is.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;

/// Whether `name`, parsed from `src` at `path`, carries `FLAG_TEST`.
fn is_test(provider: &dyn LanguageProvider, path: &str, src: &str, name: &str) -> bool {
    let local = provider
        .parse_file(path.as_ref(), src.as_bytes())
        .unwrap_or_else(|e| panic!("parse {path}: {e}"));
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    let g = builder.build();
    let pool = g.string_pool.as_slice();
    let idx = g
        .nodes
        .iter()
        .position(|n| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            ) && n.name.resolve(pool) == name
        })
        .unwrap_or_else(|| panic!("{path}: node {name} not found"));
    g.function_meta(idx as u32)
        .unwrap_or_else(|| panic!("{path}: FunctionMeta missing for {name}"))
        .is_test()
}

// ---- TypeScript ----

#[test]
fn test_function_meta_typescript_test_name_in_production_file_is_not_test() {
    let src = "export function testUrlMatchesGlob(url: string): boolean {\n    return url.length > 0;\n}\n";
    assert!(!is_test(
        &TypeScriptProvider::new().unwrap(),
        "src/urlGlob.ts",
        src,
        "testUrlMatchesGlob"
    ));
}

#[test]
fn test_function_meta_typescript_test_file_is_test() {
    let src = "export function testUrlMatchesGlob(url: string): boolean {\n    return url.length > 0;\n}\n";
    assert!(is_test(
        &TypeScriptProvider::new().unwrap(),
        "src/test/urlGlob.test.ts",
        src,
        "testUrlMatchesGlob"
    ));
}

/// vscode's `fileSearch.ts` defines a production `test()` method.
#[test]
fn test_function_meta_typescript_runner_named_method_in_production_file_is_not_test() {
    let src = "export class Matcher {\n    test(path: string): boolean {\n        return path.length > 0;\n    }\n}\n";
    assert!(!is_test(
        &TypeScriptProvider::new().unwrap(),
        "src/fileSearch.ts",
        src,
        "test"
    ));
}

// ---- JavaScript ----

#[test]
fn test_function_meta_javascript_test_name_in_production_file_is_not_test() {
    let src = "export function testConnection() {\n    return 1;\n}\n";
    assert!(!is_test(
        &JavaScriptProvider::new().unwrap(),
        "src/db.js",
        src,
        "testConnection"
    ));
}

#[test]
fn test_function_meta_javascript_test_file_is_test() {
    let src = "export function testConnection() {\n    return 1;\n}\n";
    assert!(is_test(
        &JavaScriptProvider::new().unwrap(),
        "src/db.test.js",
        src,
        "testConnection"
    ));
}

// ---- Python ----

#[test]
fn test_function_meta_python_test_name_in_production_file_is_not_test() {
    let src = "def test_connection():\n    return 1\n";
    assert!(!is_test(
        &PythonProvider::new().unwrap(),
        "app/health.py",
        src,
        "test_connection"
    ));
}

#[test]
fn test_function_meta_python_pytest_decorator_in_production_file_is_test() {
    let src = "import pytest\n\n\n@pytest.mark.slow\ndef checks_connection():\n    return 1\n";
    assert!(is_test(
        &PythonProvider::new().unwrap(),
        "app/health.py",
        src,
        "checks_connection"
    ));
}

// ---- Java ----

#[test]
fn test_function_meta_java_test_name_in_production_file_is_not_test() {
    let src = "class Health {\n    int testConnection() { return 1; }\n}\n";
    assert!(!is_test(
        &JavaProvider::new().unwrap(),
        "src/main/java/Health.java",
        src,
        "testConnection"
    ));
}

#[test]
fn test_function_meta_java_test_annotation_in_production_file_is_test() {
    let src = "class Health {\n    @Test\n    void checksConnection() {}\n}\n";
    assert!(is_test(
        &JavaProvider::new().unwrap(),
        "src/main/java/Health.java",
        src,
        "checksConnection"
    ));
}

// ---- Kotlin ----

#[test]
fn test_function_meta_kotlin_test_name_in_production_file_is_not_test() {
    let src = "fun testConnection(): Int = 1\n";
    assert!(!is_test(
        &KotlinProvider::new().unwrap(),
        "src/main/kotlin/Health.kt",
        src,
        "testConnection"
    ));
}

#[test]
fn test_function_meta_kotlin_test_annotation_in_production_file_is_test() {
    let src = "class Health {\n    @Test\n    fun checksConnection() {}\n}\n";
    assert!(is_test(
        &KotlinProvider::new().unwrap(),
        "src/main/kotlin/Health.kt",
        src,
        "checksConnection"
    ));
}

// ---- C# ----

#[test]
fn test_function_meta_csharp_test_name_in_production_file_is_not_test() {
    let src = "class Health {\n    int TestConnection() { return 1; }\n}\n";
    assert!(!is_test(
        &CSharpProvider::new().unwrap(),
        "src/Health.cs",
        src,
        "TestConnection"
    ));
}

#[test]
fn test_function_meta_csharp_fact_attribute_in_production_file_is_test() {
    let src = "class Health {\n    [Fact]\n    void ChecksConnection() {}\n}\n";
    assert!(is_test(
        &CSharpProvider::new().unwrap(),
        "src/Health.cs",
        src,
        "ChecksConnection"
    ));
}

// ---- Go ----

#[test]
fn test_function_meta_go_test_name_in_production_file_is_not_test() {
    let src = "package health\n\nfunc TestConnection() int { return 1 }\n";
    assert!(!is_test(
        &GoProvider::new().unwrap(),
        "health/health.go",
        src,
        "TestConnection"
    ));
}

#[test]
fn test_function_meta_go_test_function_in_test_file_is_test() {
    let src = "package health\n\nimport \"testing\"\n\nfunc TestConnection(t *testing.T) {}\n";
    assert!(is_test(
        &GoProvider::new().unwrap(),
        "health/health_test.go",
        src,
        "TestConnection"
    ));
}

// ---- Rust ----

#[test]
fn test_function_meta_rust_test_name_in_production_file_is_not_test() {
    let src = "pub fn test_connection() -> u32 {\n    1\n}\n";
    assert!(!is_test(
        &RustProvider::new().unwrap(),
        "src/health.rs",
        src,
        "test_connection"
    ));
}

#[test]
fn test_function_meta_rust_cfg_test_module_in_production_file_is_test() {
    let src = "pub fn connect() -> u32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn checks_connection() {\n        assert_eq!(super::connect(), 1);\n    }\n}\n";
    assert!(is_test(
        &RustProvider::new().unwrap(),
        "src/health.rs",
        src,
        "checks_connection"
    ));
}

// ---- PHP ----

#[test]
fn test_function_meta_php_test_name_in_production_file_is_not_test() {
    let src = "<?php\nfunction testimonials() {\n    return [];\n}\n";
    assert!(!is_test(
        &PhpProvider::new().unwrap(),
        "src/Pages.php",
        src,
        "testimonials"
    ));
}

#[test]
fn test_function_meta_php_test_attribute_in_production_file_is_test() {
    let src = "<?php\nclass Health {\n    #[Test]\n    public function checksConnection() {}\n}\n";
    assert!(is_test(
        &PhpProvider::new().unwrap(),
        "src/Health.php",
        src,
        "checksConnection"
    ));
}

/// Stands for: PHPUnit runs each `test*` method of a `TestCase` subclass, on
/// any path. Its non-test helper and a `test*` method of another class stay
/// production.
#[test]
fn test_function_meta_php_test_case_subclass_method_is_test() {
    let src = "<?php\nclass ParserTest extends \\PHPUnit\\Framework\\TestCase {\n    public function testParse() {}\n    public function helper() {}\n}\nclass Report extends Base {\n    public function testimonials() {}\n}\n";
    let php = PhpProvider::new().unwrap();
    assert!(is_test(&php, "src/Checks.php", src, "testParse"));
    assert!(!is_test(&php, "src/Checks.php", src, "helper"));
    assert!(!is_test(&php, "src/Checks.php", src, "testimonials"));
}

// ---- Ruby ----

#[test]
fn test_function_meta_ruby_test_name_in_production_file_is_not_test() {
    let src = "def test_connection\n  1\nend\n";
    assert!(!is_test(
        &RubyProvider::new().unwrap(),
        "lib/health.rb",
        src,
        "test_connection"
    ));
}

#[test]
fn test_function_meta_ruby_test_file_is_test() {
    let src = "def test_connection\n  1\nend\n";
    assert!(is_test(
        &RubyProvider::new().unwrap(),
        "test/health_test.rb",
        src,
        "test_connection"
    ));
}

/// Stands for: Minitest runs each `test_*` method of a `Minitest::Test`
/// subclass, on any path. Its non-test helper and a `test_*` method of
/// another class stay production.
#[test]
fn test_function_meta_ruby_minitest_subclass_method_is_test() {
    let src = "class ParserTest < Minitest::Test\n  def test_parse\n  end\n\n  def helper\n  end\nend\n\nclass Probe < Base\n  def test_connection\n  end\nend\n";
    let ruby = RubyProvider::new().unwrap();
    assert!(is_test(&ruby, "lib/checks.rb", src, "test_parse"));
    assert!(!is_test(&ruby, "lib/checks.rb", src, "helper"));
    assert!(!is_test(&ruby, "lib/checks.rb", src, "test_connection"));
}

// ---- Swift ----

#[test]
fn test_function_meta_swift_test_name_in_production_file_is_not_test() {
    let src = "func testable() -> Bool {\n    true\n}\n";
    assert!(!is_test(
        &SwiftProvider::new().unwrap(),
        "Sources/Health/Health.swift",
        src,
        "testable"
    ));
}

#[test]
fn test_function_meta_swift_test_attribute_in_production_file_is_test() {
    let src = "@Test func checksConnection() {}\n";
    assert!(is_test(
        &SwiftProvider::new().unwrap(),
        "Sources/Health/Health.swift",
        src,
        "checksConnection"
    ));
}

// ---- C ----

#[test]
fn test_function_meta_c_test_name_in_production_file_is_not_test() {
    let src = "int test_connection(void) {\n    return 1;\n}\n";
    assert!(!is_test(
        &CProvider::new().unwrap(),
        "src/health.c",
        src,
        "test_connection"
    ));
}

#[test]
fn test_function_meta_c_test_file_is_test() {
    let src = "int test_connection(void) {\n    return 1;\n}\n";
    assert!(is_test(
        &CProvider::new().unwrap(),
        "tests/test_health.c",
        src,
        "test_connection"
    ));
}

/// Stands for: XCTest runs each `test*` method of an `XCTestCase` subclass,
/// and Xcode keeps them in `<App>Tests/` folders that are no test path. Its
/// non-test helper and a `test*` method of another class stay production.
#[test]
fn test_function_meta_swift_xctestcase_subclass_method_is_test() {
    let src = "import XCTest\n\nclass ParserChecks: XCTestCase {\n    func testParse() {}\n    func helper() {}\n}\n\nclass Probe: NSObject {\n    func testConnection() {}\n}\n";
    let swift = SwiftProvider::new().unwrap();
    assert!(is_test(&swift, "Quality/Checks.swift", src, "testParse"));
    assert!(!is_test(&swift, "Quality/Checks.swift", src, "helper"));
    assert!(!is_test(
        &swift,
        "Quality/Checks.swift",
        src,
        "testConnection"
    ));
}

// ---- C++ ----

#[test]
fn test_function_meta_cpp_test_name_in_production_file_is_not_test() {
    let src = "int Test_connection() {\n    return 1;\n}\n";
    assert!(!is_test(
        &CppProvider::new().unwrap(),
        "src/health.cpp",
        src,
        "Test_connection"
    ));
}

#[test]
fn test_function_meta_cpp_test_file_is_test() {
    let src = "int Test_connection() {\n    return 1;\n}\n";
    assert!(is_test(
        &CppProvider::new().unwrap(),
        "tests/health_test.cpp",
        src,
        "Test_connection"
    ));
}

/// A Google Test macro body is evidence on any path: LLVM keeps its tests in
/// `unittests/ADT/APIntTest.cpp`.
#[test]
fn test_function_meta_cpp_gtest_macro_outside_test_path_is_test() {
    let src = "TEST(APIntTest, Add) {\n    EXPECT_EQ(1, 1);\n}\n";
    assert!(is_test(
        &CppProvider::new().unwrap(),
        "unittests/ADT/APIntTest.cpp",
        src,
        "TEST"
    ));
}

/// Stands for: the macro name alone is no evidence. A production function
/// that is named `TEST` and has a return type stays a production caller.
#[test]
fn test_function_meta_cpp_function_named_like_gtest_macro_is_not_test() {
    let src = "int TEST(int value) {\n    return value;\n}\n";
    assert!(!is_test(
        &CppProvider::new().unwrap(),
        "src/probe.cpp",
        src,
        "TEST"
    ));
}

/// Stands for: a constructor of a class named `TEST` has no return type
/// either; inline or out of line, it stays a production caller.
#[test]
fn test_function_meta_cpp_constructor_named_like_gtest_macro_is_not_test() {
    let cpp = CppProvider::new().unwrap();
    let inline = "struct TEST {\n    TEST(int value) {}\n};\n";
    assert!(!is_test(&cpp, "src/probe.cpp", inline, "TEST"));
    let out_of_line = "struct TEST {\n    TEST(int value);\n};\n\nTEST::TEST(int value) {}\n";
    assert!(!is_test(&cpp, "src/probe.cpp", out_of_line, "TEST"));
}

// ---- Dart ----

#[test]
fn test_function_meta_dart_test_name_in_production_file_is_not_test() {
    let src = "int testConnection() => 1;\n";
    assert!(!is_test(
        &DartProvider::new().unwrap(),
        "lib/health.dart",
        src,
        "testConnection"
    ));
}

#[test]
fn test_function_meta_dart_test_file_is_test() {
    let src = "int testConnection() => 1;\n";
    assert!(is_test(
        &DartProvider::new().unwrap(),
        "test/health_test.dart",
        src,
        "testConnection"
    ));
}
