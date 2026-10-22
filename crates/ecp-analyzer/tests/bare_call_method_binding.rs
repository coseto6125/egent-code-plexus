//! A receiverless call `f()` reaches a `Method` at the Global tier only in a
//! language that calls a member of the enclosing type with no receiver.
//!
//! PHP, Python, Rust and C: a bare `config()` never binds the same-named
//! method of an unrelated class (a Laravel test's private `config()` stole
//! every call of the global helper `config()`), and a free function `config`
//! is the unique Global pick even when such a method exists.
//!
//! JavaScript, TypeScript and Go keep the old Global tier: their parsers still
//! write some member calls as the bare member, and those edges must survive.
//!
//! Java, Kotlin, C#, Swift, Dart, C++ and Ruby call a member of the enclosing
//! type with no receiver: a bare call keeps reaching methods.
mod instantiation_calls_support;

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
use ecp_core::graph::ZeroCopyGraph;
use instantiation_calls_support::calls_from;
use std::path::Path;

/// `(file, owner, name)` of one `Calls` target.
type Target = (String, String, String);

fn graph(files: &[(&dyn LanguageProvider, &str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for (provider, path, src) in files {
        let local = provider
            .parse_file(Path::new(path), src.as_bytes())
            .unwrap_or_else(|e| panic!("parse {path}: {e}"));
        builder.add_graph(local);
    }
    builder.build()
}

fn single(provider: &dyn LanguageProvider, files: &[(&str, &str)]) -> ZeroCopyGraph {
    let with_provider: Vec<_> = files
        .iter()
        .map(|&(path, src)| (provider, path, src))
        .collect();
    graph(&with_provider)
}

/// The `Calls` targets of every node named `caller`, sorted.
fn targets(graph: &ZeroCopyGraph, caller: &str) -> Vec<Target> {
    let mut out: Vec<Target> = calls_from(graph, caller)
        .into_iter()
        .map(|hit| (hit.file, hit.owner, hit.name))
        .collect();
    out.sort();
    out
}

fn target(file: &str, owner: &str, name: &str) -> Target {
    (file.to_string(), owner.to_string(), name.to_string())
}

// ---- PHP: a bare call needs `$this->` / `self::` to reach a member ----

const PHP_UNRELATED: (&str, &str) = (
    "tests/MiddlewareConfigTest.php",
    "<?php\nclass MiddlewareConfigTest {\n    private function config() {}\n}\n",
);

#[test]
fn test_resolve_call_php_bare_call_unrelated_method_returns_no_edge() {
    let graph = single(
        &PhpProvider::new().unwrap(),
        &[
            PHP_UNRELATED,
            (
                "src/boot.php",
                "<?php\nfunction boot() {\n    config();\n}\n",
            ),
        ],
    );
    assert_eq!(targets(&graph, "boot"), Vec::<Target>::new());
}

#[test]
fn test_resolve_call_php_bare_call_in_class_method_unrelated_method_returns_no_edge() {
    let graph = single(
        &PhpProvider::new().unwrap(),
        &[
            PHP_UNRELATED,
            (
                "src/Ctl.php",
                "<?php\nclass Ctl {\n    public function index() {\n        return config();\n    }\n}\n",
            ),
        ],
    );
    assert_eq!(targets(&graph, "index"), Vec::<Target>::new());
}

/// Before the fix the method made the name ambiguous: no edge at all.
#[test]
fn test_resolve_call_php_bare_call_free_function_and_method_binds_free_function() {
    let graph = single(
        &PhpProvider::new().unwrap(),
        &[
            PHP_UNRELATED,
            ("src/helpers.php", "<?php\nfunction config() {}\n"),
            (
                "src/boot.php",
                "<?php\nfunction boot() {\n    config();\n}\n",
            ),
        ],
    );
    assert_eq!(
        targets(&graph, "boot"),
        vec![target("src/helpers.php", "", "config")]
    );
}

#[test]
fn test_resolve_call_php_this_member_call_binds_own_method() {
    let graph = single(
        &PhpProvider::new().unwrap(),
        &[(
            "src/K.php",
            "<?php\nclass K {\n    public function run() {\n        $this->helper();\n    }\n    private function helper() {}\n}\n",
        )],
    );
    assert_eq!(
        targets(&graph, "run"),
        vec![target("src/K.php", "K", "helper")]
    );
}

/// The class does not declare `helper`, so the parser cannot name its owner;
/// the call is still a member call and must reach the trait's method, never
/// the free function of the same name. One edge per call site.
#[test]
fn test_resolve_call_php_self_and_static_call_to_trait_method_binds_trait_method() {
    let graph = single(
        &PhpProvider::new().unwrap(),
        &[
            ("src/helpers.php", "<?php\nfunction helper() {}\n"),
            (
                "src/T.php",
                "<?php\ntrait T {\n    public function helper() {}\n}\n",
            ),
            (
                "src/K.php",
                "<?php\nclass K {\n    use T;\n    public function run() {\n        self::helper();\n        static::helper();\n    }\n}\n",
            ),
        ],
    );
    assert_eq!(
        targets(&graph, "run"),
        vec![target("src/T.php", "T", "helper"); 2]
    );
}

// ---- Python: a bare call needs `self.` to reach a member ----

const PY_UNRELATED: (&str, &str) = (
    "pkg/unrelated.py",
    "class Unrelated:\n    def config(self):\n        pass\n",
);

#[test]
fn test_resolve_call_python_bare_call_unrelated_method_returns_no_edge() {
    let graph = single(
        &PythonProvider::new().unwrap(),
        &[PY_UNRELATED, ("app.py", "def boot():\n    config()\n")],
    );
    assert_eq!(targets(&graph, "boot"), Vec::<Target>::new());
}

#[test]
fn test_resolve_call_python_bare_call_free_function_and_method_binds_free_function() {
    let graph = single(
        &PythonProvider::new().unwrap(),
        &[
            PY_UNRELATED,
            ("helpers.py", "def config():\n    pass\n"),
            ("app.py", "def boot():\n    config()\n"),
        ],
    );
    assert_eq!(
        targets(&graph, "boot"),
        vec![target("helpers.py", "", "config")]
    );
}

#[test]
fn test_resolve_call_python_self_member_call_binds_own_method() {
    let graph = single(
        &PythonProvider::new().unwrap(),
        &[(
            "k.py",
            "class K:\n    def run(self):\n        self.helper()\n\n    def helper(self):\n        pass\n",
        )],
    );
    assert_eq!(targets(&graph, "run"), vec![target("k.py", "K", "helper")]);
}

// ---- Rust: a bare call needs `self.` / `Self::` to reach a member ----

const RS_UNRELATED: (&str, &str) = (
    "src/other.rs",
    "pub struct Unrelated;\nimpl Unrelated {\n    pub fn config(&self) {}\n}\n",
);

#[test]
fn test_resolve_call_rust_bare_call_unrelated_method_returns_no_edge() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            RS_UNRELATED,
            ("src/lib.rs", "mod other;\nfn boot() {\n    config();\n}\n"),
        ],
    );
    assert_eq!(targets(&graph, "boot"), Vec::<Target>::new());
}

#[test]
fn test_resolve_call_rust_bare_call_free_function_and_method_binds_free_function() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[
            RS_UNRELATED,
            ("src/helpers.rs", "pub fn config() {}\n"),
            (
                "src/lib.rs",
                "mod helpers;\nmod other;\nfn boot() {\n    config();\n}\n",
            ),
        ],
    );
    assert_eq!(
        targets(&graph, "boot"),
        vec![target("src/helpers.rs", "", "config")]
    );
}

#[test]
fn test_resolve_call_rust_self_member_call_binds_own_method() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[(
            "src/k.rs",
            "pub struct K;\nimpl K {\n    fn run(&self) {\n        self.helper();\n    }\n    fn helper(&self) {}\n}\n",
        )],
    );
    assert_eq!(
        targets(&graph, "run"),
        vec![target("src/k.rs", "K", "helper")]
    );
}

// ---- C: no members; free-function binding is unchanged ----

#[test]
fn test_resolve_call_c_bare_call_binds_free_function() {
    let graph = single(
        &CProvider::new().unwrap(),
        &[
            ("lib.c", "void config(void) {}\n"),
            ("app.c", "void boot(void) {\n    config();\n}\n"),
        ],
    );
    assert_eq!(targets(&graph, "boot"), vec![target("lib.c", "", "config")]);
}

/// A C++ method namesake, whether or not the language barrier admits it,
/// never competes with the C free function.
#[test]
fn test_resolve_call_c_bare_call_cpp_method_namesake_binds_free_function() {
    let c: &dyn LanguageProvider = &CProvider::new().unwrap();
    let cpp: &dyn LanguageProvider = &CppProvider::new().unwrap();
    let graph = graph(&[
        (c, "lib.c", "void config(void) {}\n"),
        (
            cpp,
            "unrelated.cpp",
            "class Unrelated {\n public:\n  void config() {}\n};\n",
        ),
        (c, "app.c", "void boot(void) {\n    config();\n}\n"),
    ]);
    assert_eq!(targets(&graph, "boot"), vec![target("lib.c", "", "config")]);
}

// ---- JavaScript, TypeScript, Go: parsers still write member calls bare ----

/// Vue 2 options API: `this.shared()` in an object-literal method reaches a
/// mixin's method. The JS parser writes it as the bare `shared`, so the
/// Global tier must keep weighing methods for JavaScript.
#[test]
fn test_resolve_call_javascript_this_call_outside_class_binds_method() {
    let graph = single(
        &JavaScriptProvider::new().unwrap(),
        &[
            (
                "mixin.js",
                "export default {\n  methods: {\n    shared() {}\n  }\n};\n",
            ),
            (
                "comp.js",
                "export default {\n  methods: {\n    run() {\n      this.shared();\n    }\n  }\n};\n",
            ),
        ],
    );
    let hits = targets(&graph, "run");
    assert!(
        hits.iter()
            .any(|(file, _, name)| file == "mixin.js" && name == "shared"),
        "{hits:?}"
    );
}

/// TS writes a chained receiver (`this.svc.load()`) as the bare member.
#[test]
fn test_resolve_call_typescript_chained_member_call_binds_method() {
    let graph = single(
        &TypeScriptProvider::new().unwrap(),
        &[
            ("svc.ts", "export class Svc {\n  load() {}\n}\n"),
            (
                "app.ts",
                "export class App {\n  svc: any;\n  run() {\n    this.svc.load();\n  }\n}\n",
            ),
        ],
    );
    assert_eq!(
        targets(&graph, "run"),
        vec![target("svc.ts", "Svc", "load")]
    );
}

/// Go writes `a.store.Fetch()` on an untyped operand as the bare member.
#[test]
fn test_resolve_call_go_untyped_selector_call_binds_method() {
    let graph = single(
        &GoProvider::new().unwrap(),
        &[
            (
                "store.go",
                "package main\n\ntype Store struct{}\n\nfunc (s *Store) Fetch() {}\n",
            ),
            (
                "app.go",
                "package main\n\ntype App struct{ store *Store }\n\nfunc boot(a App) {\n\ta.store.Fetch()\n}\n",
            ),
        ],
    );
    let hits = targets(&graph, "boot");
    assert!(
        hits.iter()
            .any(|(file, owner, name)| file == "store.go" && owner == "Store" && name == "Fetch"),
        "{hits:?}"
    );
}

// ---- Implicit-`this` languages: a bare call reaches members ----

/// One implicit-`this` fixture: `own` declares class `K` whose `caller`
/// calls its own `callee` bare; `unrelated` declares `Unrelated.callee` and
/// `app` an `App.caller_app` that calls `callee` bare.
struct ImplicitThis<'a> {
    own: (&'a str, &'a str),
    caller: &'a str,
    callee: &'a str,
    unrelated: (&'a str, &'a str),
    app: (&'a str, &'a str),
    caller_app: &'a str,
}

fn assert_own_method(provider: &dyn LanguageProvider, f: &ImplicitThis<'_>) {
    let graph = single(provider, &[f.own]);
    assert_eq!(
        targets(&graph, f.caller),
        vec![target(f.own.0, "K", f.callee)]
    );
}

/// Documents the Global tier as it stands for an implicit-`this` language:
/// the unique method namesake of an unrelated class still binds. The
/// contract is "this rule leaves implicit-`this` languages alone"; tightening
/// it (admit only the enclosing type, its outer types and its bases) flips
/// this assertion on purpose.
fn assert_unrelated_method_kept(provider: &dyn LanguageProvider, f: &ImplicitThis<'_>) {
    let graph = single(provider, &[f.unrelated, f.app]);
    assert_eq!(
        targets(&graph, f.caller_app),
        vec![target(f.unrelated.0, "Unrelated", f.callee)]
    );
}

const JAVA: ImplicitThis<'static> = ImplicitThis {
    own: (
        "src/K.java",
        "class K {\n  void run() { helper(); }\n  void helper() {}\n}\n",
    ),
    caller: "run",
    callee: "helper",
    unrelated: (
        "src/Unrelated.java",
        "class Unrelated {\n  void helper() {}\n}\n",
    ),
    app: (
        "src/App.java",
        "class App {\n  void boot() { helper(); }\n}\n",
    ),
    caller_app: "boot",
};

const KOTLIN: ImplicitThis<'static> = ImplicitThis {
    own: (
        "K.kt",
        "class K {\n  fun run() { helper() }\n  fun helper() {}\n}\n",
    ),
    caller: "run",
    callee: "helper",
    unrelated: ("Unrelated.kt", "class Unrelated {\n  fun helper() {}\n}\n"),
    app: ("App.kt", "class App {\n  fun boot() { helper() }\n}\n"),
    caller_app: "boot",
};

const CSHARP: ImplicitThis<'static> = ImplicitThis {
    own: (
        "K.cs",
        "class K {\n  void Run() { Helper(); }\n  void Helper() {}\n}\n",
    ),
    caller: "Run",
    callee: "Helper",
    unrelated: (
        "Unrelated.cs",
        "class Unrelated {\n  public void Helper() {}\n}\n",
    ),
    app: ("App.cs", "class App {\n  void Boot() { Helper(); }\n}\n"),
    caller_app: "Boot",
};

const SWIFT: ImplicitThis<'static> = ImplicitThis {
    own: (
        "K.swift",
        "class K {\n  func run() { helper() }\n  func helper() {}\n}\n",
    ),
    caller: "run",
    callee: "helper",
    unrelated: (
        "Unrelated.swift",
        "class Unrelated {\n  func helper() {}\n}\n",
    ),
    app: ("App.swift", "class App {\n  func boot() { helper() }\n}\n"),
    caller_app: "boot",
};

const DART: ImplicitThis<'static> = ImplicitThis {
    own: (
        "lib/k.dart",
        "class K {\n  void run() { helper(); }\n  void helper() {}\n}\n",
    ),
    caller: "run",
    callee: "helper",
    unrelated: (
        "lib/unrelated.dart",
        "class Unrelated {\n  void helper() {}\n}\n",
    ),
    app: (
        "lib/app.dart",
        "class App {\n  void boot() { helper(); }\n}\n",
    ),
    caller_app: "boot",
};

const CPP: ImplicitThis<'static> = ImplicitThis {
    own: (
        "k.cpp",
        "class K {\n public:\n  void run() { helper(); }\n  void helper() {}\n};\n",
    ),
    caller: "run",
    callee: "helper",
    unrelated: (
        "unrelated.cpp",
        "class Unrelated {\n public:\n  void helper() {}\n};\n",
    ),
    app: (
        "app.cpp",
        "class App {\n public:\n  void boot() { helper(); }\n};\n",
    ),
    caller_app: "boot",
};

const RUBY: ImplicitThis<'static> = ImplicitThis {
    own: (
        "k.rb",
        "class K\n  def run\n    helper()\n  end\n\n  def helper\n  end\nend\n",
    ),
    caller: "run",
    callee: "helper",
    unrelated: (
        "unrelated.rb",
        "class Unrelated\n  def helper\n  end\nend\n",
    ),
    app: (
        "app.rb",
        "class App\n  def boot\n    helper()\n  end\nend\n",
    ),
    caller_app: "boot",
};

#[test]
fn test_resolve_call_java_bare_call_own_method_binds_own_method() {
    assert_own_method(&JavaProvider::new().unwrap(), &JAVA);
}

#[test]
fn test_resolve_call_java_bare_call_unrelated_method_keeps_global_bind() {
    assert_unrelated_method_kept(&JavaProvider::new().unwrap(), &JAVA);
}

#[test]
fn test_resolve_call_kotlin_bare_call_own_method_binds_own_method() {
    assert_own_method(&KotlinProvider::new().unwrap(), &KOTLIN);
}

#[test]
fn test_resolve_call_kotlin_bare_call_unrelated_method_keeps_global_bind() {
    assert_unrelated_method_kept(&KotlinProvider::new().unwrap(), &KOTLIN);
}

#[test]
fn test_resolve_call_csharp_bare_call_own_method_binds_own_method() {
    assert_own_method(&CSharpProvider::new().unwrap(), &CSHARP);
}

#[test]
fn test_resolve_call_csharp_bare_call_unrelated_method_keeps_global_bind() {
    assert_unrelated_method_kept(&CSharpProvider::new().unwrap(), &CSHARP);
}

#[test]
fn test_resolve_call_swift_bare_call_own_method_binds_own_method() {
    assert_own_method(&SwiftProvider::new().unwrap(), &SWIFT);
}

#[test]
fn test_resolve_call_swift_bare_call_unrelated_method_keeps_global_bind() {
    assert_unrelated_method_kept(&SwiftProvider::new().unwrap(), &SWIFT);
}

#[test]
fn test_resolve_call_dart_bare_call_own_method_binds_own_method() {
    assert_own_method(&DartProvider::new().unwrap(), &DART);
}

#[test]
fn test_resolve_call_dart_bare_call_unrelated_method_keeps_global_bind() {
    assert_unrelated_method_kept(&DartProvider::new().unwrap(), &DART);
}

#[test]
fn test_resolve_call_cpp_bare_call_own_method_binds_own_method() {
    assert_own_method(&CppProvider::new().unwrap(), &CPP);
}

#[test]
fn test_resolve_call_cpp_bare_call_unrelated_method_keeps_global_bind() {
    assert_unrelated_method_kept(&CppProvider::new().unwrap(), &CPP);
}

#[test]
fn test_resolve_call_ruby_bare_call_own_method_binds_own_method() {
    assert_own_method(&RubyProvider::new().unwrap(), &RUBY);
}

#[test]
fn test_resolve_call_ruby_bare_call_unrelated_method_keeps_global_bind() {
    assert_unrelated_method_kept(&RubyProvider::new().unwrap(), &RUBY);
}
