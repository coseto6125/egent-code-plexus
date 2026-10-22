//! FU-2026-10-22-211bb553d7e3: a function that names a module-level or
//! static constant bare (`LIMITS` in `readLimit()`) gets a `ReadsField` edge
//! with reason `value_read`, so `ecp impact` on the constant lists its
//! readers.
//!
//! Per language: a same-file read binds at the same-file tier; a parameter
//! of the same name binds nothing (a parameter is not the module's
//! constant); a read in another file binds at the import tier, or at the
//! global tier where the language reaches another file's name without an
//! import (Go, C, C++, Kotlin, Swift, Dart, PHP, Ruby, Rust).
//!
//! Known gaps, asserted as such so a fix flips them deliberately: a C / C++
//! array constant and a Dart `static const` have no node to bind; C# binds
//! no `using static` import.
mod name_ref_support;

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
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use name_ref_support::{
    assert_one_cross_file, graph, r, value_reads, Ref, IMPORT, SAME_FILE, VALUE_READ,
};

fn none() -> Vec<Ref> {
    Vec::new()
}

// ---- TypeScript ----

const TS_SAME_FILE: &str = "export const LIMITS = { max: 3 };\n\
export function readLimit(): number {\n  return LIMITS.max;\n}\n\
export function shadow(LIMITS: { max: number }): number {\n  return LIMITS.max;\n}\n";

#[test]
fn test_value_read_typescript_module_constant_returns_same_file_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[("a.ts", TS_SAME_FILE)],
    );
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![r("a.ts", "LIMITS", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_typescript_parameter_shadows_constant_returns_no_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[("a.ts", TS_SAME_FILE)],
    );
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_typescript_imported_constant_returns_import_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[
            ("lib.ts", "export const LIMITS = { max: 3 };\n"),
            (
                "app.ts",
                "import { LIMITS } from './lib';\nexport function readLimit(): number {\n  return LIMITS.max;\n}\n",
            ),
        ],
    );
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![r("lib.ts", "LIMITS", VALUE_READ, IMPORT)]
    );
}

/// Without an import the name cannot reach another module: no global-tier
/// guess, even when the project holds one `LIMITS`.
#[test]
fn test_value_read_typescript_unimported_constant_in_other_file_returns_no_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[
            ("lib.ts", "export const LIMITS = { max: 3 };\n"),
            (
                "app.ts",
                "export function readLimit(): number {\n  return LIMITS.max;\n}\n",
            ),
        ],
    );
    assert_eq!(value_reads(&g, "readLimit"), none());
}

// ---- JavaScript ----

const JS_SAME_FILE: &str = "export const LIMITS = { max: 3 };\n\
export function readLimit() {\n  return LIMITS.max;\n}\n\
export function shadow(LIMITS) {\n  return LIMITS.max;\n}\n\
export function local() {\n  const LIMITS = { max: 1 };\n  return LIMITS.max;\n}\n";

#[test]
fn test_value_read_javascript_module_constant_returns_same_file_edge() {
    let g = graph(
        &JavaScriptProvider::new().unwrap(),
        &[("a.js", JS_SAME_FILE)],
    );
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![r("a.js", "LIMITS", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_javascript_parameter_shadows_constant_returns_no_edge() {
    let g = graph(
        &JavaScriptProvider::new().unwrap(),
        &[("a.js", JS_SAME_FILE)],
    );
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_javascript_local_shadows_constant_returns_no_edge() {
    let g = graph(
        &JavaScriptProvider::new().unwrap(),
        &[("a.js", JS_SAME_FILE)],
    );
    assert_eq!(value_reads(&g, "local"), none());
}

#[test]
fn test_value_read_javascript_imported_constant_returns_import_edge() {
    let g = graph(
        &JavaScriptProvider::new().unwrap(),
        &[
            ("lib.js", "export const LIMITS = { max: 3 };\n"),
            (
                "app.js",
                "import { LIMITS } from './lib';\nexport function readLimit() {\n  return LIMITS.max;\n}\n",
            ),
        ],
    );
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![r("lib.js", "LIMITS", VALUE_READ, IMPORT)]
    );
}

// ---- Python ----

const PY_SAME_FILE: &str = "TIMEOUT = 5\n\n\
def g():\n    return TIMEOUT\n\n\
def shadow(TIMEOUT):\n    return TIMEOUT\n\n\
def local():\n    TIMEOUT = 1\n    return TIMEOUT\n\n\
def loop(xs):\n    return [TIMEOUT for TIMEOUT in xs]\n";

#[test]
fn test_value_read_python_module_constant_returns_same_file_edge() {
    let g = graph(&PythonProvider::new().unwrap(), &[("a.py", PY_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "g"),
        vec![r("a.py", "TIMEOUT", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_python_parameter_shadows_constant_returns_no_edge() {
    let g = graph(&PythonProvider::new().unwrap(), &[("a.py", PY_SAME_FILE)]);
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_python_local_assignment_shadows_constant_returns_no_edge() {
    let g = graph(&PythonProvider::new().unwrap(), &[("a.py", PY_SAME_FILE)]);
    assert_eq!(value_reads(&g, "local"), none());
}

#[test]
fn test_value_read_python_comprehension_variable_shadows_constant_returns_no_edge() {
    let g = graph(&PythonProvider::new().unwrap(), &[("a.py", PY_SAME_FILE)]);
    assert_eq!(value_reads(&g, "loop"), none());
}

#[test]
fn test_value_read_python_imported_constant_returns_cross_file_edge() {
    let g = graph(
        &PythonProvider::new().unwrap(),
        &[
            ("lib.py", "TIMEOUT = 5\n"),
            (
                "app.py",
                "from lib import TIMEOUT\n\ndef g():\n    return TIMEOUT\n",
            ),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "g"), "lib.py", "TIMEOUT");
}

// ---- Java ----

const JAVA_SAME_FILE: &str = "public class D {\n\
    static final int TABLE = 3;\n\
    int f() {\n        return TABLE;\n    }\n\
    int g(int TABLE) {\n        return TABLE;\n    }\n\
    int h() {\n        int TABLE = 1;\n        return TABLE;\n    }\n}\n";

#[test]
fn test_value_read_java_static_final_field_returns_same_file_edge() {
    let g = graph(&JavaProvider::new().unwrap(), &[("D.java", JAVA_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "f"),
        vec![r("D.java", "TABLE", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_java_parameter_shadows_field_returns_no_edge() {
    let g = graph(&JavaProvider::new().unwrap(), &[("D.java", JAVA_SAME_FILE)]);
    assert_eq!(value_reads(&g, "g"), none());
}

#[test]
fn test_value_read_java_local_shadows_field_returns_no_edge() {
    let g = graph(&JavaProvider::new().unwrap(), &[("D.java", JAVA_SAME_FILE)]);
    assert_eq!(value_reads(&g, "h"), none());
}

#[test]
fn test_value_read_java_static_import_returns_cross_file_edge() {
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[
            (
                "src/app/Consts.java",
                "package app;\npublic class Consts {\n    public static final int TABLE = 3;\n}\n",
            ),
            (
                "src/use/User.java",
                "package use;\nimport static app.Consts.TABLE;\npublic class User {\n    int f() {\n        return TABLE;\n    }\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "f"), "src/app/Consts.java", "TABLE");
}

// ---- Kotlin ----

const KT_SAME_FILE: &str = "val LIMITS = listOf(1, 2)\n\n\
fun readLimit(): Int {\n    return LIMITS.size\n}\n\n\
fun shadow(LIMITS: List<Int>): Int {\n    return LIMITS.size\n}\n\n\
fun local(): Int {\n    val LIMITS = listOf(1)\n    return LIMITS.size\n}\n";

#[test]
fn test_value_read_kotlin_top_level_val_returns_same_file_edge() {
    let g = graph(&KotlinProvider::new().unwrap(), &[("k.kt", KT_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![r("k.kt", "LIMITS", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_kotlin_parameter_shadows_val_returns_no_edge() {
    let g = graph(&KotlinProvider::new().unwrap(), &[("k.kt", KT_SAME_FILE)]);
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_kotlin_local_val_shadows_val_returns_no_edge() {
    let g = graph(&KotlinProvider::new().unwrap(), &[("k.kt", KT_SAME_FILE)]);
    assert_eq!(value_reads(&g, "local"), none());
}

/// A top-level `val` is visible to the whole package without an import.
#[test]
fn test_value_read_kotlin_same_package_val_returns_cross_file_edge() {
    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[
            ("app/a.kt", "package app\n\nval LIMITS = listOf(1)\n"),
            (
                "app/b.kt",
                "package app\n\nfun readLimit(): Int {\n    return LIMITS.size\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "readLimit"), "app/a.kt", "LIMITS");
}

// ---- C# ----

const CS_SAME_FILE: &str = "public class Cs {\n\
    const int Max = 3;\n\
    static readonly int[] Table = { 1 };\n\
    int F() {\n        return Max + Table[0];\n    }\n\
    int G(int Max) {\n        return Max;\n    }\n}\n";

#[test]
fn test_value_read_csharp_const_and_static_readonly_return_same_file_edges() {
    let g = graph(&CSharpProvider::new().unwrap(), &[("cs.cs", CS_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "F"),
        vec![
            r("cs.cs", "Max", VALUE_READ, SAME_FILE),
            r("cs.cs", "Table", VALUE_READ, SAME_FILE),
        ]
    );
}

#[test]
fn test_value_read_csharp_parameter_shadows_const_returns_no_edge() {
    let g = graph(&CSharpProvider::new().unwrap(), &[("cs.cs", CS_SAME_FILE)]);
    assert_eq!(value_reads(&g, "G"), none());
}

/// Known gap: a `using static` import binds no bare name, and C# drops the
/// global tier (a bare name reaches only the reader's own type).
#[test]
fn test_value_read_csharp_using_static_constant_returns_no_edge_known_gap() {
    let g = graph(
        &CSharpProvider::new().unwrap(),
        &[
            (
                "Consts.cs",
                "namespace App {\n    public static class Consts {\n        public const int Max = 3;\n    }\n}\n",
            ),
            (
                "User.cs",
                "using static App.Consts;\nnamespace App {\n    public class User {\n        int F() {\n            return Max;\n        }\n    }\n}\n",
            ),
        ],
    );
    assert_eq!(value_reads(&g, "F"), none());
}

// ---- Go ----

const GO_SAME_FILE: &str = "package main\n\n\
var Limits = []int{1, 2}\n\nconst Max = 3\n\n\
func readLimit() int {\n\treturn Limits[0] + Max\n}\n\n\
func shadow(Max int) int {\n\treturn Max\n}\n\n\
func local() int {\n\tMax := 1\n\treturn Max\n}\n";

#[test]
fn test_value_read_go_package_var_and_const_return_same_file_edges() {
    let g = graph(&GoProvider::new().unwrap(), &[("g.go", GO_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![
            r("g.go", "Limits", VALUE_READ, SAME_FILE),
            r("g.go", "Max", VALUE_READ, SAME_FILE),
        ]
    );
}

#[test]
fn test_value_read_go_parameter_shadows_const_returns_no_edge() {
    let g = graph(&GoProvider::new().unwrap(), &[("g.go", GO_SAME_FILE)]);
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_go_short_var_shadows_const_returns_no_edge() {
    let g = graph(&GoProvider::new().unwrap(), &[("g.go", GO_SAME_FILE)]);
    assert_eq!(value_reads(&g, "local"), none());
}

/// A struct literal's field name is not a read of a same-named constant.
#[test]
fn test_value_read_go_struct_literal_key_returns_no_edge() {
    let g = graph(
        &GoProvider::new().unwrap(),
        &[(
            "g.go",
            "package main\n\nconst Timeout = 3\n\ntype Cfg struct {\n\tTimeout int\n}\n\nfunc build() Cfg {\n\treturn Cfg{Timeout: 1}\n}\n",
        )],
    );
    assert_eq!(value_reads(&g, "build"), none());
}

#[test]
fn test_value_read_go_same_package_other_file_returns_cross_file_edge() {
    let g = graph(
        &GoProvider::new().unwrap(),
        &[
            ("a.go", "package main\n\nconst Max = 3\n"),
            (
                "b.go",
                "package main\n\nfunc readMax() int {\n\treturn Max\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "readMax"), "a.go", "Max");
}

// ---- Rust ----

const RS_SAME_FILE: &str = "pub const LIMIT: u32 = 3;\npub static NAME: &str = \"x\";\n\n\
pub fn read_limit() -> u32 {\n    LIMIT + NAME.len() as u32\n}\n\n\
#[allow(non_snake_case)]\npub fn shadow(LIMIT: u32) -> u32 {\n    LIMIT\n}\n\n\
pub fn pattern(xs: &[u32]) -> u32 {\n    xs.iter().map(|LIMIT| *LIMIT).sum()\n}\n";

#[test]
fn test_value_read_rust_const_and_static_return_same_file_edges() {
    let g = graph(&RustProvider::new().unwrap(), &[("src/r.rs", RS_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "read_limit"),
        vec![
            r("src/r.rs", "LIMIT", VALUE_READ, SAME_FILE),
            r("src/r.rs", "NAME", VALUE_READ, SAME_FILE),
        ]
    );
}

#[test]
fn test_value_read_rust_parameter_shadows_const_returns_no_edge() {
    let g = graph(&RustProvider::new().unwrap(), &[("src/r.rs", RS_SAME_FILE)]);
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_rust_closure_parameter_shadows_const_returns_no_edge() {
    let g = graph(&RustProvider::new().unwrap(), &[("src/r.rs", RS_SAME_FILE)]);
    assert_eq!(value_reads(&g, "pattern"), none());
}

#[test]
fn test_value_read_rust_used_const_returns_cross_file_edge() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "pub mod a;\npub mod b;\n"),
            ("src/a.rs", "pub const LIMIT: u32 = 3;\n"),
            (
                "src/b.rs",
                "use crate::a::LIMIT;\n\npub fn read_limit() -> u32 {\n    LIMIT\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "read_limit"), "src/a.rs", "LIMIT");
}

// ---- PHP ----

const PHP_SAME_FILE: &str = "<?php\nconst LIMITS = 3;\n\n\
function readLimit() {\n    return LIMITS;\n}\n\n\
function shadow($LIMITS) {\n    return $LIMITS;\n}\n";

#[test]
fn test_value_read_php_top_level_const_returns_same_file_edge() {
    let g = graph(&PhpProvider::new().unwrap(), &[("p.php", PHP_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![r("p.php", "LIMITS", VALUE_READ, SAME_FILE)]
    );
}

/// A `$variable` never names a constant.
#[test]
fn test_value_read_php_parameter_variable_returns_no_edge() {
    let g = graph(&PhpProvider::new().unwrap(), &[("p.php", PHP_SAME_FILE)]);
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_php_const_in_other_file_returns_cross_file_edge() {
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[
            ("a.php", "<?php\nconst LIMITS = 3;\n"),
            (
                "b.php",
                "<?php\nfunction readLimit() {\n    return LIMITS;\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "readLimit"), "a.php", "LIMITS");
}

// ---- Ruby ----

const RB_SAME_FILE: &str = "LIMITS = [1, 2].freeze\n\n\
def read_limit\n  LIMITS.first\nend\n\n\
def shadow(limits)\n  limits.first\nend\n\n\
class Cfg\n  MAX = 3\n\n  def max\n    MAX\n  end\nend\n";

#[test]
fn test_value_read_ruby_top_level_constant_returns_same_file_edge() {
    let g = graph(&RubyProvider::new().unwrap(), &[("rb.rb", RB_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "read_limit"),
        vec![r("rb.rb", "LIMITS", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_ruby_class_constant_returns_same_file_edge() {
    let g = graph(&RubyProvider::new().unwrap(), &[("rb.rb", RB_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "max"),
        vec![r("rb.rb", "MAX", VALUE_READ, SAME_FILE)]
    );
}

/// A Ruby local is lower-case and never a constant.
#[test]
fn test_value_read_ruby_lowercase_parameter_returns_no_edge() {
    let g = graph(&RubyProvider::new().unwrap(), &[("rb.rb", RB_SAME_FILE)]);
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_ruby_constant_in_other_file_returns_cross_file_edge() {
    let g = graph(
        &RubyProvider::new().unwrap(),
        &[
            ("a.rb", "LIMITS = [1].freeze\n"),
            ("b.rb", "def read_limit\n  LIMITS.first\nend\n"),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "read_limit"), "a.rb", "LIMITS");
}

// ---- Swift ----

const SWIFT_SAME_FILE: &str = "let limits = [1, 2]\n\n\
func readLimit() -> Int {\n    return limits.count\n}\n\n\
func shadow(limits: [Int]) -> Int {\n    return limits.count\n}\n\n\
func local() -> Int {\n    let limits = [1]\n    return limits.count\n}\n";

#[test]
fn test_value_read_swift_top_level_let_returns_same_file_edge() {
    let g = graph(
        &SwiftProvider::new().unwrap(),
        &[("s.swift", SWIFT_SAME_FILE)],
    );
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![r("s.swift", "limits", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_swift_parameter_shadows_let_returns_no_edge() {
    let g = graph(
        &SwiftProvider::new().unwrap(),
        &[("s.swift", SWIFT_SAME_FILE)],
    );
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_swift_local_let_shadows_let_returns_no_edge() {
    let g = graph(
        &SwiftProvider::new().unwrap(),
        &[("s.swift", SWIFT_SAME_FILE)],
    );
    assert_eq!(value_reads(&g, "local"), none());
}

#[test]
fn test_value_read_swift_let_in_other_file_returns_cross_file_edge() {
    let g = graph(
        &SwiftProvider::new().unwrap(),
        &[
            ("a.swift", "let limits = [1]\n"),
            (
                "b.swift",
                "func readLimit() -> Int {\n    return limits.count\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "readLimit"), "a.swift", "limits");
}

// ---- C ----

const C_SAME_FILE: &str = "static const int MAX_N = 5;\nstatic const int limits[2] = {1, 2};\n\n\
int read_limit(void) {\n    return MAX_N + limits[0];\n}\n\n\
int shadow(int MAX_N) {\n    return MAX_N;\n}\n\n\
int local(void) {\n    int MAX_N = 1;\n    return MAX_N;\n}\n";

/// Known gap: `limits` is an array declarator, which the C parser emits no
/// node for, so only `MAX_N` binds.
#[test]
fn test_value_read_c_scalar_constant_returns_edge_array_constant_known_gap() {
    let g = graph(&CProvider::new().unwrap(), &[("c1.c", C_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "read_limit"),
        vec![r("c1.c", "MAX_N", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_c_parameter_shadows_constant_returns_no_edge() {
    let g = graph(&CProvider::new().unwrap(), &[("c1.c", C_SAME_FILE)]);
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_c_local_shadows_constant_returns_no_edge() {
    let g = graph(&CProvider::new().unwrap(), &[("c1.c", C_SAME_FILE)]);
    assert_eq!(value_reads(&g, "local"), none());
}

#[test]
fn test_value_read_c_global_in_other_file_returns_cross_file_edge() {
    let g = graph(
        &CProvider::new().unwrap(),
        &[
            ("a.c", "int g_limit = 5;\n"),
            ("b.c", "int read_limit(void) {\n    return g_limit;\n}\n"),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "read_limit"), "a.c", "g_limit");
}

// ---- C++ ----

const CPP_SAME_FILE: &str = "constexpr int kMax = 5;\nstatic const int LIMITS[2] = {1, 2};\n\n\
int readLimit() {\n    return kMax + LIMITS[0];\n}\n\n\
int shadow(int kMax) {\n    return kMax;\n}\n\n\
class C {\n    static const int LIMQ = 3;\n    int g() { return LIMQ; }\n};\n";

/// Known gap: `LIMITS` is an array declarator with no node, so only `kMax`
/// binds.
#[test]
fn test_value_read_cpp_constexpr_returns_edge_array_constant_known_gap() {
    let g = graph(&CppProvider::new().unwrap(), &[("c2.cpp", CPP_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![r("c2.cpp", "kMax", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_cpp_in_class_static_const_returns_same_file_edge() {
    let g = graph(&CppProvider::new().unwrap(), &[("c2.cpp", CPP_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "g"),
        vec![r("c2.cpp", "LIMQ", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_cpp_parameter_shadows_constexpr_returns_no_edge() {
    let g = graph(&CppProvider::new().unwrap(), &[("c2.cpp", CPP_SAME_FILE)]);
    assert_eq!(value_reads(&g, "shadow"), none());
}

#[test]
fn test_value_read_cpp_header_constant_returns_cross_file_edge() {
    let g = graph(
        &CppProvider::new().unwrap(),
        &[
            ("a.h", "constexpr int kMax = 5;\n"),
            (
                "b.cpp",
                "#include \"a.h\"\nint readMax() {\n    return kMax;\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "readMax"), "a.h", "kMax");
}

// ---- Dart ----

const DART_SAME_FILE: &str = "const limits = [1, 2];\n\n\
int readLimit() {\n  return limits.length;\n}\n\n\
int shadow(List<int> limits) {\n  return limits.length;\n}\n\n\
class D2 {\n  static const limq = 3;\n  int g() {\n    return limq;\n  }\n}\n";

#[test]
fn test_value_read_dart_top_level_const_returns_same_file_edge() {
    let g = graph(&DartProvider::new().unwrap(), &[("d.dart", DART_SAME_FILE)]);
    assert_eq!(
        value_reads(&g, "readLimit"),
        vec![r("d.dart", "limits", VALUE_READ, SAME_FILE)]
    );
}

#[test]
fn test_value_read_dart_parameter_shadows_const_returns_no_edge() {
    let g = graph(&DartProvider::new().unwrap(), &[("d.dart", DART_SAME_FILE)]);
    assert_eq!(value_reads(&g, "shadow"), none());
}

/// Known gap: the Dart parser emits no node for a `static const` class
/// member, so its bare read binds nothing.
#[test]
fn test_value_read_dart_static_const_returns_no_edge_known_gap() {
    let g = graph(&DartProvider::new().unwrap(), &[("d.dart", DART_SAME_FILE)]);
    assert_eq!(value_reads(&g, "g"), none());
}

#[test]
fn test_value_read_dart_imported_const_returns_cross_file_edge() {
    let g = graph(
        &DartProvider::new().unwrap(),
        &[
            ("a.dart", "const limits = [1];\n"),
            (
                "b.dart",
                "import 'a.dart';\n\nint readLimit() {\n  return limits.length;\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&value_reads(&g, "readLimit"), "a.dart", "limits");
}
