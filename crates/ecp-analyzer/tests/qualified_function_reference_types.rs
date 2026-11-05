//! A function named through a type or namespace qualifier and passed as a
//! value (C# `xs.ForEach(Cb.M)`, C++ `for_each(.., ns::f)`, Swift
//! `xs.map(Cb.f)`, Dart `xs.map(Cb.f)`, PHP `array_map([Cb::class, 'm'], ..)`,
//! Ruby `Cb.method(:m)`) gets a `References` edge with reason
//! `function_reference`, like the bare form. The qualifier picks the
//! declaration: with `Cb.M` and `Other.M` in the tree, `Cb.M` binds only `Cb`.
//!
//! Contract of every language: a callee records no `function_reference`
//! edge, a qualifier that is a typed variable or parameter binds the method
//! its declared type owns (never a same-named class), an untyped one binds
//! nothing, and a qualifier that names nothing binds nothing.
//!
//! Syntax a language does not have, so no test:
//! - C++ `obj::f` cannot name a variable (`::` takes a namespace or a type),
//!   so there is no local-qualifier case.
//! - PHP and Ruby have no bare-name value read of a variable qualifier
//!   (`$obj` is a skipped variable name; Ruby reads constants only), so those
//!   local-qualifier tests assert the missing edge alone.
//! - C has no qualified name; `&f` is the only value shape.
use crate::name_ref_support;

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use name_ref_support::{assert_one_cross_file, calls, function_refs, graph, value_reads, Ref};

fn refs(provider: &dyn LanguageProvider, files: &[(&str, &str)], registrar: &str) -> Vec<Ref> {
    function_refs(&graph(provider, files), registrar)
}

fn none() -> Vec<Ref> {
    Vec::new()
}

fn reads_name(edges: &[Ref], name: &str) -> bool {
    edges.iter().any(|(_, target, _, _)| target == name)
}

// ---- C# ----

fn csharp() -> CSharpProvider {
    CSharpProvider::new().unwrap()
}

const CS_CB: &str = "public class Cb {\n    public static void M(string s) {}\n}\n";
const CS_OTHER: &str = "public class Other {\n    public static void M(string s) {}\n}\n";

fn cs_use(body: &str) -> String {
    format!(
        "using System.Collections.Generic;\npublic class Use {{\n    Cb held;\n    public void Run(List<string> xs, Cb p) {{\n        {body}\n    }}\n}}\n"
    )
}

#[test]
fn test_qualified_function_reference_csharp_method_group_argument_binds() {
    let used = cs_use("xs.ForEach(Cb.M);");
    let found = refs(
        &csharp(),
        &[("Cb.cs", CS_CB), ("Use.cs", used.as_str())],
        "Run",
    );
    assert_one_cross_file(&found, "Cb.cs", "M");
}

#[test]
fn test_qualified_function_reference_csharp_callee_returns_no_edge() {
    let used = cs_use("Cb.M(\"x\");");
    let g = graph(&csharp(), &[("Cb.cs", CS_CB), ("Use.cs", used.as_str())]);
    assert_eq!(function_refs(&g, "Run"), none());
    assert_eq!(
        calls(&g, "Run"),
        vec![("Cb.cs".to_string(), "M".to_string())]
    );
}

/// Stands for: A parameter typed `Cb` binds `c.M` to `Cb.M`.
#[test]
fn test_qualified_function_reference_csharp_parameter_qualifier_binds_parameter_type() {
    let used = cs_use("xs.ForEach(p.M);");
    let found = refs(
        &csharp(),
        &[("Cb.cs", CS_CB), ("Use.cs", used.as_str())],
        "Run",
    );
    assert_one_cross_file(&found, "Cb.cs", "M");
}

/// Stands for: `held.M` names `M` on the field's declared type, and the field read stays.
#[test]
fn test_qualified_function_reference_csharp_field_qualifier_binds_field_type_and_keeps_value_read()
{
    let used = cs_use("xs.ForEach(held.M);");
    let g = graph(&csharp(), &[("Cb.cs", CS_CB), ("Use.cs", used.as_str())]);
    assert_one_cross_file(&function_refs(&g, "Run"), "Cb.cs", "M");
    assert!(
        reads_name(&value_reads(&g, "Run"), "held"),
        "{:?}",
        value_reads(&g, "Run")
    );
}

#[test]
fn test_qualified_function_reference_csharp_this_qualifier_returns_no_edge() {
    let used = cs_use("xs.ForEach(this.M);");
    let found = refs(
        &csharp(),
        &[("Cb.cs", CS_CB), ("Use.cs", used.as_str())],
        "Run",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_csharp_same_member_in_two_types_binds_named_type() {
    let used = cs_use("xs.ForEach(Other.M);");
    let found = refs(
        &csharp(),
        &[
            ("Cb.cs", CS_CB),
            ("Other.cs", CS_OTHER),
            ("Use.cs", used.as_str()),
        ],
        "Run",
    );
    assert_one_cross_file(&found, "Other.cs", "M");
}

#[test]
fn test_qualified_function_reference_csharp_unresolvable_qualifier_returns_no_edge() {
    let used = cs_use("xs.ForEach(Missing.M);");
    let found = refs(
        &csharp(),
        &[("Cb.cs", CS_CB), ("Use.cs", used.as_str())],
        "Run",
    );
    assert_eq!(found, none());
}

const CS_LIB_CB: &str =
    "namespace Lib {\n    public class Cb {\n        public static void M(string s) {}\n    }\n}\n";

/// The qualifier `Lib.Cb` is the namespace and type path that encloses `M`.
#[test]
fn test_qualified_function_reference_csharp_namespace_qualified_type_binds() {
    let used = cs_use("xs.ForEach(Lib.Cb.M);");
    let found = refs(
        &csharp(),
        &[("Cb.cs", CS_LIB_CB), ("Use.cs", used.as_str())],
        "Run",
    );
    assert_one_cross_file(&found, "Cb.cs", "M");
}

/// `Lib.Cb.M` names a `Cb` that the namespace `Lib` holds: a `Cb` outside
/// any namespace is another type, and binds nothing.
#[test]
fn test_qualified_function_reference_csharp_namespace_the_type_lacks_returns_no_edge() {
    let used = cs_use("xs.ForEach(Lib.Cb.M);");
    let found = refs(
        &csharp(),
        &[("Cb.cs", CS_CB), ("Use.cs", used.as_str())],
        "Run",
    );
    assert_eq!(found, none());
}

// ---- C++ ----

fn cpp() -> CppProvider {
    CppProvider::new().unwrap()
}

const CPP_NS: &str = "namespace ns {\nint f(int x) { return x; }\n}\n";
const CPP_OTHER_NS: &str = "namespace other {\nint f(int x) { return x; }\n}\n";
const CPP_CLS: &str = "struct Cb {\n    static int sf(int x) { return x; }\n};\n";

fn cpp_use(body: &str) -> String {
    format!("#include <algorithm>\n#include <vector>\nvoid apply(std::vector<int>& xs) {{\n    {body}\n}}\n")
}

#[test]
fn test_qualified_function_reference_cpp_namespace_function_argument_binds() {
    let used = cpp_use("std::for_each(xs.begin(), xs.end(), ns::f);");
    let found = refs(
        &cpp(),
        &[("ns.cpp", CPP_NS), ("use.cpp", used.as_str())],
        "apply",
    );
    assert_one_cross_file(&found, "ns.cpp", "f");
}

#[test]
fn test_qualified_function_reference_cpp_address_of_namespace_function_binds() {
    let used = cpp_use("std::for_each(xs.begin(), xs.end(), &ns::f);");
    let found = refs(
        &cpp(),
        &[("ns.cpp", CPP_NS), ("use.cpp", used.as_str())],
        "apply",
    );
    assert_one_cross_file(&found, "ns.cpp", "f");
}

#[test]
fn test_qualified_function_reference_cpp_static_member_argument_binds() {
    let used = cpp_use("std::for_each(xs.begin(), xs.end(), Cb::sf);");
    let found = refs(
        &cpp(),
        &[("cb.cpp", CPP_CLS), ("use.cpp", used.as_str())],
        "apply",
    );
    assert_one_cross_file(&found, "cb.cpp", "sf");
}

#[test]
fn test_qualified_function_reference_cpp_callee_returns_no_edge() {
    let used = cpp_use("ns::f(1);\n    Cb::sf(2);");
    let g = graph(
        &cpp(),
        &[
            ("ns.cpp", CPP_NS),
            ("cb.cpp", CPP_CLS),
            ("use.cpp", used.as_str()),
        ],
    );
    assert_eq!(function_refs(&g, "apply"), none());
}

#[test]
fn test_qualified_function_reference_cpp_same_member_in_two_namespaces_binds_named_one() {
    let used = cpp_use("std::for_each(xs.begin(), xs.end(), other::f);");
    let found = refs(
        &cpp(),
        &[
            ("ns.cpp", CPP_NS),
            ("other.cpp", CPP_OTHER_NS),
            ("use.cpp", used.as_str()),
        ],
        "apply",
    );
    assert_one_cross_file(&found, "other.cpp", "f");
}

#[test]
fn test_qualified_function_reference_cpp_unresolvable_qualifier_returns_no_edge() {
    let used = cpp_use("std::for_each(xs.begin(), xs.end(), missing::f);");
    let found = refs(
        &cpp(),
        &[("ns.cpp", CPP_NS), ("use.cpp", used.as_str())],
        "apply",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_cpp_base_class_clause_returns_no_edge() {
    let g = graph(
        &cpp(),
        &[
            ("ns.cpp", "namespace ns {\nstruct Base { Base() {} };\n}\n"),
            (
                "use.cpp",
                "struct Derived : public ns::Base {\n    void run() {}\n};\n",
            ),
        ],
    );
    assert_eq!(function_refs(&g, "Derived"), none());
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_qualified_function_reference_cpp_still_reads_bare_value_beside_scoped_name() {
    let g = graph(
        &cpp(),
        &[
            ("ns.cpp", CPP_NS),
            (
                "use.cpp",
                "int limit = 3;\nint apply2() {\n    return ns::f(limit);\n}\n",
            ),
        ],
    );
    assert!(
        reads_name(&value_reads(&g, "apply2"), "limit"),
        "{:?}",
        value_reads(&g, "apply2")
    );
}

// ---- Swift ----

fn swift() -> SwiftProvider {
    SwiftProvider::new().unwrap()
}

const SWIFT_CB: &str = "struct Cb {\n    static func f(_ x: Int) -> Int { return x }\n}\n";
const SWIFT_OTHER: &str = "struct Other {\n    static func f(_ x: Int) -> Int { return x }\n}\n";

fn swift_use(body: &str) -> String {
    format!(
        "struct Use {{\n    let held = Cb()\n    func apply(_ xs: [Int], _ p: Cb) -> [Int] {{\n        {body}\n    }}\n}}\n"
    )
}

#[test]
fn test_qualified_function_reference_swift_type_function_argument_binds() {
    let used = swift_use("return xs.map(Cb.f)");
    let found = refs(
        &swift(),
        &[("Cb.swift", SWIFT_CB), ("Use.swift", used.as_str())],
        "apply",
    );
    assert_one_cross_file(&found, "Cb.swift", "f");
}

#[test]
fn test_qualified_function_reference_swift_callee_returns_no_edge() {
    let used = swift_use("return [Cb.f(1)]");
    let g = graph(
        &swift(),
        &[("Cb.swift", SWIFT_CB), ("Use.swift", used.as_str())],
    );
    assert_eq!(function_refs(&g, "apply"), none());
}

/// Stands for: A parameter typed `Cb` binds `c.f` to `Cb.f`.
#[test]
fn test_qualified_function_reference_swift_parameter_qualifier_binds_parameter_type() {
    let used = swift_use("return xs.map(p.f)");
    let found = refs(
        &swift(),
        &[("Cb.swift", SWIFT_CB), ("Use.swift", used.as_str())],
        "apply",
    );
    assert_one_cross_file(&found, "Cb.swift", "f");
}

#[test]
fn test_qualified_function_reference_swift_property_qualifier_keeps_value_read() {
    let used = swift_use("return xs.map(held.f)");
    let g = graph(
        &swift(),
        &[("Cb.swift", SWIFT_CB), ("Use.swift", used.as_str())],
    );
    assert_eq!(function_refs(&g, "apply"), none());
    assert!(
        reads_name(&value_reads(&g, "apply"), "held"),
        "{:?}",
        value_reads(&g, "apply")
    );
}

#[test]
fn test_qualified_function_reference_swift_self_qualifier_returns_no_edge() {
    let used = swift_use("return xs.map(self.f)");
    let found = refs(
        &swift(),
        &[("Cb.swift", SWIFT_CB), ("Use.swift", used.as_str())],
        "apply",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_swift_same_member_in_two_types_binds_named_type() {
    let used = swift_use("return xs.map(Other.f)");
    let found = refs(
        &swift(),
        &[
            ("Cb.swift", SWIFT_CB),
            ("Other.swift", SWIFT_OTHER),
            ("Use.swift", used.as_str()),
        ],
        "apply",
    );
    assert_one_cross_file(&found, "Other.swift", "f");
}

#[test]
fn test_qualified_function_reference_swift_unresolvable_qualifier_returns_no_edge() {
    let used = swift_use("return xs.map(Missing.f)");
    let found = refs(
        &swift(),
        &[("Cb.swift", SWIFT_CB), ("Use.swift", used.as_str())],
        "apply",
    );
    assert_eq!(found, none());
}

// ---- Dart ----

fn dart() -> DartProvider {
    DartProvider::new().unwrap()
}

const DART_CB: &str = "class Cb {\n  static int f(int x) => x;\n}\n";
const DART_OTHER: &str = "class Other {\n  static int f(int x) => x;\n}\n";

fn dart_use(body: &str) -> String {
    format!(
        "import 'cb.dart';\nimport 'other.dart';\nfinal Cb held = Cb();\nList<int> apply(List<int> xs, Cb p) {{\n  {body}\n}}\n"
    )
}

fn dart_files(used: &str) -> [(&str, &str); 3] {
    [
        ("cb.dart", DART_CB),
        ("other.dart", DART_OTHER),
        ("use.dart", used),
    ]
}

#[test]
fn test_qualified_function_reference_dart_class_function_argument_binds() {
    let used = dart_use("return xs.map(Cb.f).toList();");
    let found = refs(&dart(), &dart_files(&used), "apply");
    assert_one_cross_file(&found, "cb.dart", "f");
}

#[test]
fn test_qualified_function_reference_dart_callee_returns_no_edge() {
    let used = dart_use("return [Cb.f(1)];");
    let g = graph(&dart(), &dart_files(&used));
    assert_eq!(function_refs(&g, "apply"), none());
}

/// Stands for: A parameter typed `Cb` binds `c.f` to `Cb.f`.
#[test]
fn test_qualified_function_reference_dart_parameter_qualifier_binds_parameter_type() {
    let used = dart_use("return xs.map(p.f).toList();");
    let found = refs(&dart(), &dart_files(&used), "apply");
    assert_one_cross_file(&found, "cb.dart", "f");
}

#[test]
fn test_qualified_function_reference_dart_variable_qualifier_keeps_value_read() {
    let used = dart_use("return xs.map(held.f).toList();");
    let g = graph(&dart(), &dart_files(&used));
    assert_eq!(function_refs(&g, "apply"), none());
    assert!(
        reads_name(&value_reads(&g, "apply"), "held"),
        "{:?}",
        value_reads(&g, "apply")
    );
}

#[test]
fn test_qualified_function_reference_dart_this_qualifier_returns_no_edge() {
    let used = dart_use("return xs.map(this.f).toList();");
    let found = refs(&dart(), &dart_files(&used), "apply");
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_dart_same_member_in_two_classes_binds_named_class() {
    let used = dart_use("return xs.map(Other.f).toList();");
    let found = refs(&dart(), &dart_files(&used), "apply");
    assert_one_cross_file(&found, "other.dart", "f");
}

#[test]
fn test_qualified_function_reference_dart_unresolvable_qualifier_returns_no_edge() {
    let used = dart_use("return xs.map(Missing.f).toList();");
    let found = refs(&dart(), &dart_files(&used), "apply");
    assert_eq!(found, none());
}

// ---- PHP ----

fn php() -> PhpProvider {
    PhpProvider::new().unwrap()
}

const PHP_UTIL: &str =
    "<?php\nclass Util {\n    public static function run($x) { return $x; }\n}\n";
const PHP_OTHER: &str =
    "<?php\nclass Other {\n    public static function run($x) { return $x; }\n}\n";

fn php_use(body: &str) -> String {
    format!("<?php\nfunction apply($xs, $obj) {{\n    {body}\n}}\n")
}

#[test]
fn test_qualified_function_reference_php_array_callable_argument_binds() {
    let used = php_use("return array_map([Util::class, 'run'], $xs);");
    let found = refs(
        &php(),
        &[("Util.php", PHP_UTIL), ("use.php", used.as_str())],
        "apply",
    );
    assert_one_cross_file(&found, "Util.php", "run");
}

#[test]
fn test_qualified_function_reference_php_string_static_callable_argument_binds() {
    let used = php_use("return array_map('Util::run', $xs);");
    let found = refs(
        &php(),
        &[("Util.php", PHP_UTIL), ("use.php", used.as_str())],
        "apply",
    );
    assert_one_cross_file(&found, "Util.php", "run");
}

#[test]
fn test_qualified_function_reference_php_array_callable_in_usort_second_position_binds() {
    let used = php_use("usort($xs, [Util::class, 'run']);");
    let found = refs(
        &php(),
        &[("Util.php", PHP_UTIL), ("use.php", used.as_str())],
        "apply",
    );
    assert_one_cross_file(&found, "Util.php", "run");
}

#[test]
fn test_qualified_function_reference_php_callee_returns_no_edge() {
    let used = php_use("return Util::run($xs);");
    let g = graph(
        &php(),
        &[("Util.php", PHP_UTIL), ("use.php", used.as_str())],
    );
    assert_eq!(function_refs(&g, "apply"), none());
}

#[test]
fn test_qualified_function_reference_php_object_qualifier_returns_no_edge() {
    let used = php_use("return array_map([$obj, 'run'], $xs);");
    let found = refs(
        &php(),
        &[("Util.php", PHP_UTIL), ("use.php", used.as_str())],
        "apply",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_php_same_member_in_two_classes_binds_named_class() {
    let used = php_use("return array_map([Other::class, 'run'], $xs);");
    let found = refs(
        &php(),
        &[
            ("Util.php", PHP_UTIL),
            ("Other.php", PHP_OTHER),
            ("use.php", used.as_str()),
        ],
        "apply",
    );
    assert_one_cross_file(&found, "Other.php", "run");
}

#[test]
fn test_qualified_function_reference_php_unresolvable_class_returns_no_edge() {
    let used = php_use("return array_map([Missing::class, 'run'], $xs);");
    let found = refs(
        &php(),
        &[("Util.php", PHP_UTIL), ("use.php", used.as_str())],
        "apply",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_php_array_with_three_elements_returns_no_edge() {
    let used = php_use("return array_map([Util::class, 'run', 'extra'], $xs);");
    let found = refs(
        &php(),
        &[("Util.php", PHP_UTIL), ("use.php", used.as_str())],
        "apply",
    );
    assert_eq!(found, none());
}

// ---- Ruby ----

fn ruby() -> RubyProvider {
    RubyProvider::new().unwrap()
}

const RUBY_UTIL: &str = "module Util\n  def self.run(x)\n    x\n  end\nend\n";
const RUBY_OTHER: &str = "module Other\n  def self.run(x)\n    x\n  end\nend\n";

fn ruby_use(body: &str) -> String {
    format!("class Svc\n  def wire(items, obj)\n    {body}\n  end\nend\n")
}

#[test]
fn test_qualified_function_reference_ruby_module_method_object_binds() {
    let used = ruby_use("items.each(&Util.method(:run))");
    let found = refs(
        &ruby(),
        &[("util.rb", RUBY_UTIL), ("svc.rb", used.as_str())],
        "wire",
    );
    assert_one_cross_file(&found, "util.rb", "run");
}

/// `Lib::Util` is the module path that encloses `run`.
#[test]
fn test_qualified_function_reference_ruby_scoped_constant_receiver_binds_enclosed_module() {
    let used = ruby_use("items.each(&Lib::Util.method(:run))");
    let found = refs(
        &ruby(),
        &[
            (
                "util.rb",
                "module Lib\n  module Util\n    def self.run(x)\n      x\n    end\n  end\nend\n",
            ),
            ("svc.rb", used.as_str()),
        ],
        "wire",
    );
    assert_one_cross_file(&found, "util.rb", "run");
}

/// `Lib::Util` names a `Util` inside `Lib`: a top-level `Util` is another
/// module, and binds nothing.
#[test]
fn test_qualified_function_reference_ruby_scoped_receiver_outside_its_module_returns_no_edge() {
    let used = ruby_use("items.each(&Lib::Util.method(:run))");
    let found = refs(
        &ruby(),
        &[("util.rb", RUBY_UTIL), ("svc.rb", used.as_str())],
        "wire",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_ruby_callee_returns_no_edge() {
    let used = ruby_use("items.each { |x| Util.run(x) }");
    let g = graph(
        &ruby(),
        &[("util.rb", RUBY_UTIL), ("svc.rb", used.as_str())],
    );
    assert_eq!(function_refs(&g, "wire"), none());
}

#[test]
fn test_qualified_function_reference_ruby_variable_receiver_returns_no_edge() {
    let used = ruby_use("items.each(&obj.method(:run))");
    let found = refs(
        &ruby(),
        &[("util.rb", RUBY_UTIL), ("svc.rb", used.as_str())],
        "wire",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_ruby_same_member_in_two_modules_binds_named_module() {
    let used = ruby_use("items.each(&Other.method(:run))");
    let found = refs(
        &ruby(),
        &[
            ("util.rb", RUBY_UTIL),
            ("other.rb", RUBY_OTHER),
            ("svc.rb", used.as_str()),
        ],
        "wire",
    );
    assert_one_cross_file(&found, "other.rb", "run");
}

#[test]
fn test_qualified_function_reference_ruby_unresolvable_receiver_returns_no_edge() {
    let used = ruby_use("items.each(&Missing.method(:run))");
    let found = refs(
        &ruby(),
        &[("util.rb", RUBY_UTIL), ("svc.rb", used.as_str())],
        "wire",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_ruby_self_receiver_matches_bare_form() {
    let src = "class Svc\n  def handler(x)\n    x\n  end\n\n  def wire(items)\n    items.each(&self.method(:handler))\n  end\nend\n";
    let found = refs(&ruby(), &[("svc.rb", src)], "wire");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].1, "handler");
}

// ---- C, Java, Kotlin: already bind; regression only ----

#[test]
fn test_qualified_function_reference_c_address_of_function_argument_binds() {
    let found = refs(
        &CProvider::new().unwrap(),
        &[
            ("cmp.c", "int cmp(const void *a, const void *b) {\n    return 0;\n}\n"),
            (
                "use.c",
                "#include <stdlib.h>\n\nvoid sort_all(int *xs, int n) {\n    qsort(xs, n, sizeof(int), &cmp);\n}\n",
            ),
        ],
        "sort_all",
    );
    assert_one_cross_file(&found, "cmp.c", "cmp");
}

const JAVA_CB: &str = "class Cb {\n    static void print(String s) {}\n}\n";
const JAVA_OTHER: &str = "class Other {\n    static void print(String s) {}\n}\n";

fn java_use(body: &str) -> String {
    format!(
        "class Use {{\n    void run(java.util.List<String> xs, Cb cb) {{\n        {body}\n    }}\n}}\n"
    )
}

#[test]
fn test_qualified_function_reference_java_class_method_reference_binds() {
    let used = java_use("xs.forEach(Cb::print);");
    let found = refs(
        &JavaProvider::new().unwrap(),
        &[("Cb.java", JAVA_CB), ("Use.java", used.as_str())],
        "run",
    );
    assert_one_cross_file(&found, "Cb.java", "print");
}

#[test]
fn test_qualified_function_reference_java_same_member_in_two_classes_binds_named_class() {
    let used = java_use("xs.forEach(Other::print);");
    let found = refs(
        &JavaProvider::new().unwrap(),
        &[
            ("Cb.java", JAVA_CB),
            ("Other.java", JAVA_OTHER),
            ("Use.java", used.as_str()),
        ],
        "run",
    );
    assert_one_cross_file(&found, "Other.java", "print");
}

/// Stands for: A parameter typed `Cb` binds `c::print` to `Cb::print`.
#[test]
fn test_qualified_function_reference_java_parameter_qualifier_binds_parameter_type() {
    let used = java_use("xs.forEach(cb::print);");
    let found = refs(
        &JavaProvider::new().unwrap(),
        &[("Cb.java", JAVA_CB), ("Use.java", used.as_str())],
        "run",
    );
    assert_one_cross_file(&found, "Cb.java", "print");
}

#[test]
fn test_qualified_function_reference_java_callee_returns_no_edge() {
    let used = java_use("Cb.print(\"x\");");
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[("Cb.java", JAVA_CB), ("Use.java", used.as_str())],
    );
    assert_eq!(function_refs(&g, "run"), none());
}

const KOTLIN_CB: &str = "object Cb {\n    fun print(s: String) {}\n}\n";
const KOTLIN_OTHER: &str = "object Other {\n    fun print(s: String) {}\n}\n";

#[test]
fn test_qualified_function_reference_kotlin_object_callable_reference_binds() {
    let found = refs(
        &KotlinProvider::new().unwrap(),
        &[
            ("Cb.kt", KOTLIN_CB),
            (
                "use.kt",
                "fun wire(xs: List<String>) {\n    xs.forEach(Cb::print)\n}\n",
            ),
        ],
        "wire",
    );
    assert_one_cross_file(&found, "Cb.kt", "print");
}

#[test]
fn test_qualified_function_reference_kotlin_same_member_in_two_objects_binds_named_object() {
    let found = refs(
        &KotlinProvider::new().unwrap(),
        &[
            ("Cb.kt", KOTLIN_CB),
            ("Other.kt", KOTLIN_OTHER),
            (
                "use.kt",
                "fun wire(xs: List<String>) {\n    xs.forEach(Other::print)\n}\n",
            ),
        ],
        "wire",
    );
    assert_one_cross_file(&found, "Other.kt", "print");
}

#[test]
fn test_qualified_function_reference_kotlin_callee_returns_no_edge() {
    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[
            ("Cb.kt", KOTLIN_CB),
            ("use.kt", "fun wire() {\n    Cb.print(\"x\")\n}\n"),
        ],
    );
    assert_eq!(function_refs(&g, "wire"), none());
}

#[test]
fn test_qualified_function_reference_dart_enum_value_argument_returns_no_edge() {
    let used =
        "import 'color.dart';\nvoid sink(Color c) {}\nvoid apply() {\n  sink(Color.red);\n}\n";
    let g = graph(
        &dart(),
        &[
            ("color.dart", "enum Color { red, blue }\n"),
            ("use.dart", used),
        ],
    );
    assert_eq!(function_refs(&g, "apply"), none());
    assert!(!reads_name(&value_reads(&g, "apply"), "red"));
}

// ---- A parameter named like the type qualifier (F4 item 1) ----
// Ruby and PHP cannot write it: a Ruby constant is never a local, and a PHP
// local carries its `$`. C++ `ns::f` and Rust `m::f` scope through a
// namespace or module that no local can stand for.

/// Stands for: A parameter `Other Cb` types `Cb.M` as `Other.M`, never the class `Cb`.
#[test]
fn test_qualified_function_reference_csharp_parameter_named_like_type_binds_parameter_type() {
    let used = "using System.Collections.Generic;\npublic class Use {\n    public void Run(List<string> xs, Other Cb) {\n        xs.ForEach(Cb.M);\n    }\n    public void Ok(List<string> xs) {\n        xs.ForEach(Cb.M);\n    }\n}\n";
    let g = graph(
        &csharp(),
        &[("Cb.cs", CS_CB), ("Other.cs", CS_OTHER), ("Use.cs", used)],
    );
    assert_one_cross_file(&function_refs(&g, "Run"), "Other.cs", "M");
    assert_one_cross_file(&function_refs(&g, "Ok"), "Cb.cs", "M");
}

/// Stands for: A parameter `Cb: Other` types `Cb.f` as `Other.f`, never the type `Cb`.
#[test]
fn test_qualified_function_reference_swift_parameter_named_like_type_binds_parameter_type() {
    let used = "struct Use {\n    func apply(_ xs: [Int], _ Cb: Other) -> [Int] {\n        return xs.map(Cb.f)\n    }\n    func ok(_ xs: [Int]) -> [Int] {\n        return xs.map(Cb.f)\n    }\n}\n";
    let g = graph(
        &swift(),
        &[
            ("Cb.swift", SWIFT_CB),
            ("Other.swift", SWIFT_OTHER),
            ("Use.swift", used),
        ],
    );
    assert_one_cross_file(&function_refs(&g, "apply"), "Other.swift", "f");
    assert_one_cross_file(&function_refs(&g, "ok"), "Cb.swift", "f");
}

/// Stands for: A parameter `Other Cb` types `Cb.f` as `Other.f`, never the class `Cb`.
#[test]
fn test_qualified_function_reference_dart_parameter_named_like_class_binds_parameter_type() {
    let used = "import 'cb.dart';\nimport 'other.dart';\nList<int> apply(List<int> xs, Other Cb) {\n  return xs.map(Cb.f).toList();\n}\nList<int> ok(List<int> xs) {\n  return xs.map(Cb.f).toList();\n}\n";
    let g = graph(&dart(), &dart_files(used));
    assert_one_cross_file(&function_refs(&g, "apply"), "other.dart", "f");
    assert_one_cross_file(&function_refs(&g, "ok"), "cb.dart", "f");
}

/// Stands for: A parameter `Other Cb` types `Cb::print` as `Other::print`, never the class `Cb`.
#[test]
fn test_qualified_function_reference_java_parameter_named_like_class_binds_parameter_type() {
    let used = "class Use {\n    void run(java.util.List<String> xs, Other Cb) {\n        xs.forEach(Cb::print);\n    }\n    void ok(java.util.List<String> xs) {\n        xs.forEach(Cb::print);\n    }\n}\n";
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[
            ("Cb.java", JAVA_CB),
            ("Other.java", JAVA_OTHER),
            ("Use.java", used),
        ],
    );
    assert_one_cross_file(&function_refs(&g, "run"), "Other.java", "print");
    assert_one_cross_file(&function_refs(&g, "ok"), "Cb.java", "print");
}

/// Stands for: A parameter `Cb: Other` types `Cb::print` as `Other::print`, never the object `Cb`.
#[test]
fn test_qualified_function_reference_kotlin_parameter_named_like_object_binds_parameter_type() {
    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[
            ("Cb.kt", KOTLIN_CB),
            ("Other.kt", KOTLIN_OTHER),
            (
                "use.kt",
                "fun wire(xs: List<String>, Cb: Other) {\n    xs.forEach(Cb::print)\n}\nfun ok(xs: List<String>) {\n    xs.forEach(Cb::print)\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&function_refs(&g, "wire"), "Other.kt", "print");
    assert_one_cross_file(&function_refs(&g, "ok"), "Cb.kt", "print");
}

// ---- A multi-segment qualifier (F4 item 2) ----
// The whole qualifier is kept, and it binds only as the namespace and type
// path that encloses the function. Python, TS, JS and Go record a
// one-segment qualifier only; PHP and C name no such path.

/// `B` declares its own `Helpers.Format`; `A.Helpers.Format` names `A`'s, so
/// `B`'s never binds.
#[test]
fn test_qualified_function_reference_csharp_namespace_path_skips_callers_namesake() {
    let a = "namespace A {\n    public static class Helpers {\n        public static string Format(string s) => s;\n    }\n}\n";
    let b = "using System.Collections.Generic;\nnamespace B {\n    static class Helpers {\n        public static string Format(string s) => s;\n    }\n    class Use {\n        void Run(List<string> xs) {\n            xs.ForEach(A.Helpers.Format);\n        }\n    }\n}\n";
    let found = refs(&csharp(), &[("A/Helpers.cs", a), ("B/Use.cs", b)], "Run");
    assert!(
        found.iter().all(|(file, _, _, _)| file != "B/Use.cs"),
        "{found:?}"
    );
}

/// `System.Console.WriteLine` names the framework's `Console`: a project
/// `Console` outside any `System` namespace binds nothing.
#[test]
fn test_qualified_function_reference_csharp_framework_path_skips_project_namesake() {
    let console = "public class Console {\n    public static void WriteLine(string s) {}\n}\n";
    let used = cs_use("xs.ForEach(System.Console.WriteLine);");
    let found = refs(
        &csharp(),
        &[("Console.cs", console), ("Use.cs", used.as_str())],
        "Run",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_csharp_nested_type_path_binds() {
    let lib = "namespace Lib {\n    public class Outer {\n        public class Inner {\n            public static void M(string s) {}\n        }\n    }\n}\n";
    let found = refs(
        &csharp(),
        &[
            ("Lib/Outer.cs", lib),
            ("Use.cs", cs_use("xs.ForEach(Lib.Outer.Inner.M);").as_str()),
        ],
        "Run",
    );
    assert_one_cross_file(&found, "Lib/Outer.cs", "M");
}

#[test]
fn test_qualified_function_reference_csharp_non_ascii_namespace_path_binds() {
    let lib = "namespace Bibliothèque {\n    public class Çb {\n        public static void M(string s) {}\n    }\n}\n";
    let found = refs(
        &csharp(),
        &[
            ("Çb.cs", lib),
            ("Use.cs", cs_use("xs.ForEach(Bibliothèque.Çb.M);").as_str()),
        ],
        "Run",
    );
    assert_one_cross_file(&found, "Çb.cs", "M");
}

#[test]
fn test_qualified_function_reference_cpp_nested_namespace_path_binds() {
    let used = cpp_use("std::for_each(xs.begin(), xs.end(), a::detail::helper);");
    let found = refs(
        &cpp(),
        &[
            (
                "a.cpp",
                "namespace a {\nnamespace detail {\nint helper(int x) { return x; }\n}\n}\n",
            ),
            ("use.cpp", used.as_str()),
        ],
        "apply",
    );
    assert_one_cross_file(&found, "a.cpp", "helper");
}

/// The caller's file declares `b::detail::helper`; `a::detail::helper`
/// names `a`'s, so `b`'s never binds.
#[test]
fn test_qualified_function_reference_cpp_namespace_path_skips_callers_namesake() {
    let used = "#include <algorithm>\n#include <vector>\nnamespace b {\nnamespace detail {\nint helper(int x) { return x; }\n}\n}\nvoid apply(std::vector<int>& xs) {\n    std::for_each(xs.begin(), xs.end(), a::detail::helper);\n}\n";
    let found = refs(
        &cpp(),
        &[
            (
                "a.cpp",
                "namespace a {\nnamespace detail {\nint helper(int x) { return x; }\n}\n}\n",
            ),
            ("b.cpp", used),
        ],
        "apply",
    );
    assert!(
        found.iter().all(|(file, _, _, _)| file != "b.cpp"),
        "{found:?}"
    );
}

#[test]
fn test_qualified_function_reference_cpp_namespace_static_member_path_binds() {
    let used = cpp_use("std::for_each(xs.begin(), xs.end(), ns::Cb::sf);");
    let found = refs(
        &cpp(),
        &[
            (
                "cb.cpp",
                "namespace ns {\nstruct Cb {\n    static int sf(int x) { return x; }\n};\n}\n",
            ),
            ("use.cpp", used.as_str()),
        ],
        "apply",
    );
    assert_one_cross_file(&found, "cb.cpp", "sf");
}

const SWIFT_NESTED: &str =
    "enum Outer {\n    enum Inner {\n        static func f(_ x: Int) -> Int { return x }\n    }\n}\n";

#[test]
fn test_qualified_function_reference_swift_nested_type_path_binds() {
    let used = swift_use("return xs.map(Outer.Inner.f)");
    let found = refs(
        &swift(),
        &[("Outer.swift", SWIFT_NESTED), ("Use.swift", used.as_str())],
        "apply",
    );
    assert_one_cross_file(&found, "Outer.swift", "f");
}

#[test]
fn test_qualified_function_reference_swift_wrong_outer_type_returns_no_edge() {
    let used = swift_use("return xs.map(Wrong.Inner.f)");
    let found = refs(
        &swift(),
        &[("Outer.swift", SWIFT_NESTED), ("Use.swift", used.as_str())],
        "apply",
    );
    assert_eq!(found, none());
}

/// Dart has no nested types: a two-segment qualifier is an import prefix
/// and a class, and the prefix encloses nothing.
#[test]
fn test_qualified_function_reference_dart_import_prefix_path_binds() {
    let used = "import 'cb.dart' as p;\nList<int> apply(List<int> xs) {\n  return xs.map(p.Cb.f).toList();\n}\n";
    let found = refs(
        &dart(),
        &[("cb.dart", DART_CB), ("use.dart", used)],
        "apply",
    );
    assert_one_cross_file(&found, "cb.dart", "f");
}

#[test]
fn test_qualified_function_reference_ruby_scoped_receiver_skips_callers_namesake() {
    let used = "module Util\n  def self.run(x)\n    x\n  end\nend\nclass Svc\n  def wire(items)\n    items.each(&Lib::Util.method(:run))\n  end\nend\n";
    let found = refs(
        &ruby(),
        &[
            (
                "util.rb",
                "module Lib\n  module Util\n    def self.run(x)\n      x\n    end\n  end\nend\n",
            ),
            ("svc.rb", used),
        ],
        "wire",
    );
    assert!(
        found.iter().all(|(file, _, _, _)| file != "svc.rb"),
        "{found:?}"
    );
}

const JAVA_PKG_CB: &str =
    "package com.example;\n\nclass Cb {\n    static void print(String s) {}\n    static class Inner {\n        static void m(String s) {}\n    }\n}\n";

#[test]
fn test_qualified_function_reference_java_package_qualified_reference_binds() {
    let used = java_use("xs.forEach(com.example.Cb::print);");
    let found = refs(
        &JavaProvider::new().unwrap(),
        &[
            ("com/example/Cb.java", JAVA_PKG_CB),
            ("Use.java", used.as_str()),
        ],
        "run",
    );
    assert_one_cross_file(&found, "com/example/Cb.java", "print");
}

#[test]
fn test_qualified_function_reference_java_other_package_returns_no_edge() {
    let used = java_use("xs.forEach(org.other.Cb::print);");
    let found = refs(
        &JavaProvider::new().unwrap(),
        &[
            ("com/example/Cb.java", JAVA_PKG_CB),
            ("Use.java", used.as_str()),
        ],
        "run",
    );
    assert_eq!(found, none());
}

#[test]
fn test_qualified_function_reference_java_nested_class_reference_binds() {
    let used = java_use("xs.forEach(Cb.Inner::m);");
    let found = refs(
        &JavaProvider::new().unwrap(),
        &[
            ("com/example/Cb.java", JAVA_PKG_CB),
            ("Use.java", used.as_str()),
        ],
        "run",
    );
    assert_one_cross_file(&found, "com/example/Cb.java", "m");
}

// ---- PHP relative callables (F4 item 8) ----
// `[$this, 'm']`, `[self::class, 'm']` and `[static::class, 'm']` name the
// enclosing class's method, as `call_user_func` reads them.

const PHP_SORTER: &str = "<?php\nclass Sorter {\n    function cmp($a, $b) { return 0; }\n    function byThis($r) { usort($r, [$this, 'cmp']); }\n    function bySelf($r) { usort($r, [self::class, 'cmp']); }\n    function byStatic($r) { usort($r, [static::class, 'cmp']); }\n}\nclass Other {\n    function cmp($a, $b) { return 1; }\n}\n";

#[test]
fn test_qualified_function_reference_php_relative_callables_bind_enclosing_class() {
    let g = graph(&php(), &[("Sorter.php", PHP_SORTER)]);
    for registrar in ["byThis", "bySelf", "byStatic"] {
        let found = function_refs(&g, registrar);
        assert_eq!(found.len(), 1, "{registrar}: {found:?}");
        assert_eq!(
            (found[0].0.as_str(), found[0].1.as_str()),
            ("Sorter.php", "cmp"),
            "{registrar}: {found:?}"
        );
    }
}

#[test]
fn test_qualified_function_reference_php_this_callable_outside_class_returns_no_edge() {
    let src = "<?php\nclass Sorter {\n    function cmp($a, $b) { return 0; }\n}\nfunction loose($r) { usort($r, [$this, 'cmp']); }\n";
    let g = graph(&php(), &[("Sorter.php", src)]);
    assert_eq!(function_refs(&g, "loose"), none());
}

/// `$this` in a trait is the class that uses it: only the trait's own
/// method is reachable through the trait's name.
#[test]
fn test_qualified_function_reference_php_trait_this_callable_binds_own_method_only() {
    let src = "<?php\ntrait Sorts {\n    function cmp($a, $b) { return 0; }\n    function own($r) { usort($r, [$this, 'cmp']); }\n    function host($r) { usort($r, [$this, 'hostCmp']); }\n}\nclass Host {\n    use Sorts;\n    function hostCmp($a, $b) { return 0; }\n}\n";
    let g = graph(&php(), &[("Sorts.php", src)]);
    let own = function_refs(&g, "own");
    assert_eq!(own.len(), 1, "{own:?}");
    assert_eq!((own[0].0.as_str(), own[0].1.as_str()), ("Sorts.php", "cmp"));
    assert_eq!(function_refs(&g, "host"), none());
}

#[test]
fn test_qualified_function_reference_php_this_callable_in_anonymous_class_returns_no_edge() {
    let src = "<?php\nclass Sorter {\n    function cmp($a, $b) { return 0; }\n    function make() {\n        return new class {\n            function sortAll($r) { usort($r, [$this, 'cmp']); }\n        };\n    }\n}\n";
    let g = graph(&php(), &[("Sorter.php", src)]);
    assert_eq!(function_refs(&g, "sortAll"), none());
}
