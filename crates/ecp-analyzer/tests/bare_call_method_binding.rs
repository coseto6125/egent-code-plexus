//! A receiverless call `f()` reaches a `Method` only in a language that
//! calls a member of the enclosing type with no receiver.
//!
//! PHP, Python, Rust and C: a bare `config()` never binds the same-named
//! method of an unrelated class (a Laravel test's private `config()` stole
//! every call of the global helper `config()`), and a free function `config`
//! is the unique Global pick even when such a method exists. Nor does it bind
//! the caller's own method or its base's: the same-file tier weighs no
//! method, and the heritage tier does not run.
//!
//! JavaScript, TypeScript and Go require an explicit receiver. Their parsers
//! preserve member syntax for receiver calls, so only genuine bare calls use
//! the free-function tier.
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
use ecp_core::analyzer::path_aliases::PathAliases;
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

fn go_module(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let tmp = tempfile::tempdir().expect("tempdir");
    let provider = GoProvider::new().unwrap();
    // Mirrors the alias that the CLI derives from this fixture's go.mod.
    let mut aliases = PathAliases::new();
    aliases.add("example.com/app/*", vec!["./*".to_string()]);
    let mut builder = GraphBuilder::new()
        .with_repo_root(tmp.path().to_path_buf())
        .with_path_aliases(aliases);
    for &(path, source) in files {
        let disk_path = tmp.path().join(path);
        std::fs::create_dir_all(disk_path.parent().expect("parent")).expect("create parent");
        std::fs::write(&disk_path, source).expect("write fixture");
        if path.ends_with(".go") {
            builder.add_graph(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .expect("parse Go fixture"),
            );
        }
    }
    builder.build()
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

#[test]
fn test_resolve_call_nested_free_function_precedes_top_level_namesake() {
    let rust = RustProvider::new().unwrap();
    let python = PythonProvider::new().unwrap();
    let cases: &[(&dyn LanguageProvider, &str, &str)] = &[
        (
            &rust,
            "src/lib.rs",
            "fn outer() { fn helper() {} fn inner() { helper(); } } fn helper() {}",
        ),
        (
            &python,
            "main.py",
            "def outer():\n    def helper():\n        pass\n    def inner():\n        helper()\ndef helper():\n    pass\n",
        ),
    ];
    for &(provider, path, source) in cases {
        let graph = single(provider, &[(path, source)]);
        assert_eq!(
            targets(&graph, "inner"),
            vec![target(path, "outer", "helper")],
            "{path}"
        );
    }
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
fn test_resolve_call_rust_bare_call_in_inline_module_binds_module_function() {
    let source = r#"
mod imp {
    struct Config;
    impl Config {
        fn load() {}
        fn run_inner() { load(); }
    }
    fn load() {}
}
struct Top;
impl Top {
    fn load() {}
    fn run_root() { load(); }
}
fn load() {}
"#;
    let graph = single(&RustProvider::new().unwrap(), &[("src/lib.rs", source)]);
    assert_eq!(
        targets(&graph, "run_inner"),
        vec![target("src/lib.rs", "imp", "load")]
    );
    assert_eq!(
        targets(&graph, "run_root"),
        vec![target("src/lib.rs", "", "load")]
    );
}

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

// ---- Same-file and heritage tiers: a bare call skips the caller's members ----

/// `config()` is Laravel's global helper, not the class's own `config`.
#[test]
fn test_resolve_call_php_bare_call_same_file_method_returns_no_edge() {
    let graph = single(
        &PhpProvider::new().unwrap(),
        &[(
            "src/Settings.php",
            "<?php\nclass Settings {\n    protected function config($k) {}\n    public function boot() {\n        return config('app.name');\n    }\n}\n",
        )],
    );
    assert_eq!(targets(&graph, "boot"), Vec::<Target>::new());
}

/// The parent's `config` needs `$this->` / `parent::` too.
#[test]
fn test_resolve_call_php_bare_call_parent_method_returns_no_edge() {
    let graph = single(
        &PhpProvider::new().unwrap(),
        &[
            (
                "src/Base.php",
                "<?php\nclass Base {\n    protected function config($k) {}\n}\n",
            ),
            (
                "src/Child.php",
                "<?php\nclass Child extends Base {\n    public function boot() {\n        return config('app.name');\n    }\n}\n",
            ),
        ],
    );
    assert_eq!(targets(&graph, "boot"), Vec::<Target>::new());
}

/// `run` is `subprocess.run`, a library function: no edge, and never the
/// class's own `run`.
#[test]
fn test_resolve_call_python_bare_call_same_file_method_library_import_returns_no_edge() {
    let graph = single(
        &PythonProvider::new().unwrap(),
        &[(
            "job.py",
            "from subprocess import run\n\n\nclass Job:\n    def run(self):\n        pass\n\n    def build(self):\n        run([\"make\"])\n",
        )],
    );
    assert_eq!(targets(&graph, "build"), Vec::<Target>::new());
}

#[test]
fn test_resolve_call_python_bare_call_same_file_method_project_import_binds_import() {
    let graph = single(
        &PythonProvider::new().unwrap(),
        &[
            ("tools.py", "def run(cmd):\n    pass\n"),
            (
                "job.py",
                "from tools import run\n\n\nclass Job:\n    def run(self):\n        pass\n\n    def build(self):\n        run([\"make\"])\n",
            ),
        ],
    );
    assert_eq!(
        targets(&graph, "build"),
        vec![target("tools.py", "", "run")]
    );
}

/// Inside `impl Config`, `load(p)` calls the free `load`: `Config::load`
/// needs `Self::`. Before, Tier 1 picked `Config::load`, and the
/// self-recursion skip then dropped the call.
#[test]
fn test_resolve_call_rust_bare_call_same_file_method_binds_free_function() {
    let graph = single(
        &RustProvider::new().unwrap(),
        &[(
            "src/lib.rs",
            "pub struct Config;\n\nimpl Config {\n    pub fn load(p: &str) -> Self {\n        load(p);\n        Config\n    }\n}\n\npub fn load(p: &str) {}\n",
        )],
    );
    assert_eq!(
        targets(&graph, "load"),
        vec![target("src/lib.rs", "", "load")]
    );
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

#[test]
fn test_resolve_call_javascript_options_this_call_binds_same_object_method() {
    let graph = single(
        &JavaScriptProvider::new().unwrap(),
        &[(
            "comp.js",
            "export default {\n  methods: {\n    shared() {},\n    run() { this.shared(); }\n  }\n};\n",
        )],
    );
    assert_eq!(
        targets(&graph, "run"),
        vec![target("comp.js", "", "shared")]
    );
}

#[test]
fn test_resolve_call_javascript_bare_call_excludes_own_and_unrelated_methods() {
    let provider = JavaScriptProvider::new().unwrap();
    for files in [
        vec![("app.js", "class App { helper() {} run() { helper(); } }\n")],
        vec![
            ("other.js", "class Other { helper() {} }\n"),
            ("app.js", "function run() { helper(); }\n"),
        ],
    ] {
        assert!(targets(&single(&provider, &files), "run").is_empty());
    }
}

#[test]
fn test_resolve_call_typescript_bare_call_excludes_own_and_unrelated_methods() {
    let provider = TypeScriptProvider::new().unwrap();
    for files in [
        vec![("app.ts", "class App { helper() {} run() { helper(); } }\n")],
        vec![
            ("other.ts", "class Other { helper() {} }\n"),
            ("app.ts", "function run() { helper(); }\n"),
        ],
    ] {
        assert!(targets(&single(&provider, &files), "run").is_empty());
    }
}

#[test]
fn test_resolve_call_go_bare_call_excludes_own_and_unrelated_methods() {
    let provider = GoProvider::new().unwrap();
    for files in [
        vec![("app.go", "package app\ntype App struct {}\nfunc (a App) Helper() {}\nfunc (a App) run() { Helper() }\n")],
        vec![
            ("other.go", "package app\ntype Other struct {}\nfunc (o Other) Helper() {}\n"),
            ("app.go", "package app\nfunc run() { Helper() }\n"),
        ],
    ] {
        assert!(targets(&single(&provider, &files), "run").is_empty());
    }
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

/// Go writes `a.store.Fetch()` on an untyped operand as an untyped member.
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

#[test]
fn test_resolve_call_go_imported_package_and_alias_calls_bind_function() {
    for (import, caller) in [
        ("\"example.com/app/pkg\"", "pkg.F()"),
        ("p \"example.com/app/pkg\"", "p.F()"),
    ] {
        let graph = go_module(&[
            ("go.mod", "module example.com/app\n\ngo 1.21\n"),
            ("pkg/helper.go", "package pkg\n\nfunc F() {}\n"),
            (
                "cmd/app.go",
                &format!("package main\n\nimport {import}\n\nfunc boot() {{ {caller} }}\n"),
            ),
        ]);
        assert_eq!(
            targets(&graph, "boot"),
            vec![target("pkg/helper.go", "", "F")]
        );
    }
}

#[test]
fn test_resolve_call_go_declared_package_name_differs_from_import_path_binds_function() {
    for (path, package, import, caller) in [
        ("v2/helper.go", "app", "\"example.com/app/v2\"", "app.F()"),
        (
            "internal/go-utils/helper.go",
            "utils",
            "\"example.com/app/internal/go-utils\"",
            "utils.F()",
        ),
    ] {
        let graph = go_module(&[
            ("go.mod", "module example.com/app\n\ngo 1.21\n"),
            (path, &format!("package {package}\n\nfunc F() {{}}\n")),
            (
                "cmd/app.go",
                &format!("package main\n\nimport {import}\n\nfunc boot() {{ {caller} }}\n"),
            ),
        ]);
        assert_eq!(
            targets(&graph, "boot"),
            vec![target(path, "", "F")],
            "{path}"
        );
    }
}

#[test]
fn test_resolve_call_go_local_and_parameter_shadow_import_alias_return_no_edge() {
    let graph = go_module(
        &[
            ("go.mod", "module example.com/app\n\ngo 1.21\n"),
            ("pkg/helper.go", "package pkg\n\nfunc F() {}\n"),
            (
                "cmd/app.go",
                "package main\n\nimport p \"example.com/app/pkg\"\n\nfunc parameter(p struct{}) { p.F() }\n\nfunc local() { p := struct{}{}; p.F() }\n",
            ),
        ],
    );
    assert!(targets(&graph, "parameter").is_empty());
    assert!(targets(&graph, "local").is_empty());
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
