use ecp_analyzer::flow::{
    analyze, Budgets, Direction, FlowReport, FlowRequest, SourceFile, Subject,
};

fn source(path: &str, text: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        source: text.into(),
    }
}

fn slice(files: &[SourceFile], file: &str, marker: &str, direction: Direction) -> FlowReport {
    let text = &files.iter().find(|item| item.path == file).unwrap().source;
    let offset = text.find(marker).expect("unique source marker");
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap().len() + 1;
    analyze(
        files,
        &FlowRequest {
            file: file.into(),
            line,
            column,
            subject: Subject::Value,
            direction,
            budgets: Budgets::default(),
        },
    )
    .unwrap()
}

fn line_of(file: &SourceFile, marker: &str) -> usize {
    let offset = file.source.find(marker).expect("unique source marker");
    file.source[..offset].matches('\n').count() + 1
}

fn forward(files: &[SourceFile], file: &str, marker: &str) -> FlowReport {
    slice(files, file, marker, Direction::Forward)
}

fn binding(report: &FlowReport, file: &str, label: &str) -> bool {
    report
        .nodes
        .iter()
        .any(|node| node.file == file && node.kind == "binding" && node.label == label)
}

fn argument(report: &FlowReport, label: &str) -> bool {
    report
        .nodes
        .iter()
        .any(|node| node.kind == "argument" && node.label == label)
}

const UPLOAD_FILE: &str = r#"<?php

namespace App\Support;

class UploadFile
{
    public static function upload($folder, $file)
    {
        return "public/{$folder}";
    }
}
"#;

const OTHER: &str = r#"<?php

namespace App\Support;

class Other
{
    public static function take($given)
    {
        return $given;
    }
}
"#;

fn controller(imports: &str, body: &str) -> String {
    format!(
        r#"<?php

namespace App\Http\Controllers;

{imports}

class FileController extends Controller
{{
    public function upload(Request $request)
    {{
        $fileDir = $request->get('dir');
{body}
    }}
}}
"#
    )
}

fn corpus(controller: String) -> Vec<SourceFile> {
    vec![
        source("app/Http/Controllers/FileController.php", &controller),
        source("app/Support/UploadFile.php", UPLOAD_FILE),
        source("app/Support/Other.php", OTHER),
    ]
}

const CONTROLLER: &str = "app/Http/Controllers/FileController.php";
const UPLOAD: &str = "app/Support/UploadFile.php";

#[test]
fn test_analyze_php_imported_static_call_reaches_callee_parameter() {
    let files = corpus(controller(
        "use App\\Support\\UploadFile;\nuse Illuminate\\Http\\Request;",
        "        $url = UploadFile::upload($fileDir, $request);\n        return $url;",
    ));
    let report = forward(&files, CONTROLLER, "$fileDir =");
    assert!(
        binding(&report, UPLOAD, "$folder"),
        "the argument must bind the callee parameter in the other file: {report:#?}"
    );
    assert!(
        report
            .nodes
            .iter()
            .any(|node| node.file == UPLOAD && node.kind == "return"),
        "the callee return must be reached: {report:#?}"
    );
    assert!(
        report.boundaries.iter().any(|b| b.kind == "import"
            && b.file == CONTROLLER
            && b.message.contains("not declared")),
        "a vendor import stays an explicit boundary: {:?}",
        report.boundaries
    );
}

#[test]
fn test_analyze_php_aliased_import_reaches_callee_parameter() {
    let files = corpus(controller(
        "use App\\Support\\UploadFile as Uploader;",
        "        return Uploader::upload($fileDir, null);",
    ));
    let report = forward(&files, CONTROLLER, "$fileDir =");
    assert!(binding(&report, UPLOAD, "$folder"), "{report:#?}");
}

#[test]
fn test_analyze_php_grouped_import_binds_every_clause() {
    let files = corpus(controller(
        "use App\\Support\\{UploadFile, Other as Renamed};",
        "        $a = UploadFile::upload($fileDir, null);\n        return Renamed::take($fileDir);",
    ));
    let report = forward(&files, CONTROLLER, "$fileDir =");
    assert!(binding(&report, UPLOAD, "$folder"), "{report:#?}");
    assert!(
        binding(&report, "app/Support/Other.php", "$given"),
        "the aliased clause of a group must bind too: {report:#?}"
    );
}

#[test]
fn test_analyze_php_unresolved_class_keeps_boundaries() {
    let files = corpus(controller(
        "use Vendor\\Missing;\nuse function App\\helper;",
        "        return Missing::go($fileDir);",
    ));
    let report = forward(&files, CONTROLLER, "$fileDir =");
    let call = line_of(&files[0], "Missing::go");
    assert!(
        report
            .boundaries
            .iter()
            .any(|b| b.kind == "external_call" && b.file == CONTROLLER && b.line == call),
        "the unresolved call keeps its boundary: {:?}",
        report.boundaries
    );
    assert!(
        report
            .boundaries
            .iter()
            .any(|b| b.kind == "import" && b.message.contains("not declared")),
        "{:?}",
        report.boundaries
    );
    assert!(
        report
            .boundaries
            .iter()
            .any(|b| b.kind == "import" && b.message.contains("function and constant")),
        "{:?}",
        report.boundaries
    );
    assert!(
        report.nodes.iter().all(|node| node.file == CONTROLLER),
        "no callee may be invented: {report:#?}"
    );
}

#[test]
fn test_analyze_php_missing_static_method_keeps_external_call() {
    let files = corpus(controller(
        "use App\\Support\\UploadFile;",
        "        return UploadFile::absent($fileDir);",
    ));
    let report = forward(&files, CONTROLLER, "$fileDir =");
    let call = line_of(&files[0], "UploadFile::absent");
    assert!(
        report
            .boundaries
            .iter()
            .any(|b| b.kind == "external_call" && b.line == call),
        "{:?}",
        report.boundaries
    );
    assert!(!binding(&report, UPLOAD, "$folder"), "{report:#?}");
}

#[test]
fn test_analyze_php_same_namespace_class_resolves_without_use() {
    let files = [
        source(
            "a.php",
            "<?php\nnamespace App;\nclass A\n{\n    public static function id($s) { return $s; }\n}\n",
        ),
        source("b.php", "<?php\nnamespace App;\nconsume(A::id(101));\n"),
    ];
    let report = forward(&files, "b.php", "101");
    assert!(binding(&report, "a.php", "$s"), "{report:#?}");
    assert!(argument(&report, "A::id(101)"), "{report:#?}");
}

#[test]
fn test_analyze_php_namespace_less_class_resolves_by_name() {
    let files = [
        source(
            "lib.php",
            "<?php\nclass Lib\n{\n    public static function id($x) { return $x; }\n}\n",
        ),
        source("main.php", "<?php\n$a = Lib::id(101);\nconsume($a);\n"),
    ];
    let report = forward(&files, "main.php", "101");
    assert!(binding(&report, "lib.php", "$x"), "{report:#?}");
    assert!(argument(&report, "$a"), "{report:#?}");
}

#[test]
fn test_analyze_php_braced_namespace_class_resolves_through_use() {
    let files = [
        source(
            "tool.php",
            "<?php\nnamespace App\\Lib {\n    class Tool\n    {\n        public static function id($t) { return $t; }\n    }\n}\n",
        ),
        source(
            "main.php",
            "<?php\nuse App\\Lib\\Tool;\nconsume(Tool::id(101));\n",
        ),
    ];
    let report = forward(&files, "main.php", "101");
    assert!(binding(&report, "tool.php", "$t"), "{report:#?}");
}

const BOX: &str = r#"<?php
class Box
{
    public static function inner($v) { return $v; }
    public static function viaSelf($a) { return self::inner($a); }
    public static function viaStatic($b) { return static::inner($b); }
    public function run() { $seed = 303; consume($this->inner($seed)); }
}
consume(Box::viaSelf(101));
consume(Box::viaStatic(202));
"#;

#[test]
fn test_analyze_php_self_call_reaches_same_class_method() {
    let files = [source("box.php", BOX)];
    let report = forward(&files, "box.php", "101");
    assert!(binding(&report, "box.php", "$v"), "{report:#?}");
    assert!(argument(&report, "Box::viaSelf(101)"), "{report:#?}");
}

#[test]
fn test_analyze_php_static_call_reaches_same_class_method() {
    let files = [source("box.php", BOX)];
    let report = forward(&files, "box.php", "202");
    assert!(binding(&report, "box.php", "$v"), "{report:#?}");
    assert!(argument(&report, "Box::viaStatic(202)"), "{report:#?}");
}

#[test]
fn test_analyze_php_this_call_reaches_same_class_method() {
    let files = [source("box.php", BOX)];
    let report = forward(&files, "box.php", "303");
    assert!(binding(&report, "box.php", "$v"), "{report:#?}");
    assert!(argument(&report, "$this->inner($seed)"), "{report:#?}");
}

#[test]
fn test_analyze_php_this_call_includes_subclass_override() {
    let files = [source(
        "hook.php",
        r#"<?php
class Base
{
    public function run() { $seed = 101; return $this->hook($seed); }
    public function hook($inBase) { return $inBase; }
}
class Child extends Base
{
    public function hook($inChild) { return $inChild; }
}
"#,
    )];
    let report = forward(&files, "hook.php", "101");
    assert!(binding(&report, "hook.php", "$inBase"), "{report:#?}");
    assert!(
        binding(&report, "hook.php", "$inChild"),
        "late binding may dispatch to the override: {report:#?}"
    );
}

#[test]
fn test_analyze_php_parent_and_inherited_calls_reach_base_method() {
    let files = [source(
        "inherit.php",
        r#"<?php
class P
{
    public static function id($p) { return $p; }
}
class C extends P
{
    public static function run() { return parent::id(101); }
}
consume(C::id(202));
"#,
    )];
    let via_parent = forward(&files, "inherit.php", "101");
    assert!(binding(&via_parent, "inherit.php", "$p"), "{via_parent:#?}");
    let inherited = forward(&files, "inherit.php", "202");
    assert!(binding(&inherited, "inherit.php", "$p"), "{inherited:#?}");
    assert!(argument(&inherited, "C::id(202)"), "{inherited:#?}");
}

#[test]
fn test_analyze_php_closure_argument_reaches_closure_parameter() {
    let files = [source(
        "closure.php",
        r#"<?php
class Runner
{
    public static function apply($callback, $value) { return $callback($value); }
}
$result = Runner::apply(function ($item) { return $item; }, 101);
consume($result);
"#,
    )];
    let report = forward(&files, "closure.php", "101");
    assert!(binding(&report, "closure.php", "$item"), "{report:#?}");
    assert!(argument(&report, "$result"), "{report:#?}");
}

#[test]
fn test_analyze_php_recursive_static_call_terminates_at_fixed_point() {
    let files = [source(
        "walk.php",
        r#"<?php
class Walker
{
    public static function down($n)
    {
        if ($n) {
            return self::down($n);
        }
        return $n;
    }
}
consume(Walker::down(101));
"#,
    )];
    let report = forward(&files, "walk.php", "101");
    assert!(argument(&report, "Walker::down(101)"), "{report:#?}");
    assert!(
        report
            .boundaries
            .iter()
            .any(|b| b.kind == "recursive_summary"),
        "{:?}",
        report.boundaries
    );
}

#[test]
fn test_analyze_php_method_named_like_builtin_is_not_builtin() {
    let files = [source(
        "count.php",
        "<?php\n$n = 101;\nconsume($list->count($n));\n",
    )];
    let report = forward(&files, "count.php", "101");
    assert!(
        !report.boundaries.iter().any(|b| b.kind == "builtin_model"),
        "{:?}",
        report.boundaries
    );
    assert!(
        !argument(&report, "$list->count($n)"),
        "an unresolved method must not pass its argument through: {report:#?}"
    );
}

#[test]
fn test_analyze_php_backward_boundaries_skip_structural_nodes() {
    let files = corpus(controller(
        "use App\\Support\\UploadFile;\nuse Illuminate\\Http\\Request;",
        "        $tmp = new Request();\n        try {\n            $url = UploadFile::upload($fileDir, $tmp);\n        } catch (\\Exception $e) {\n            $url = null;\n        }\n        return $url;",
    ));
    let report = slice(&files, CONTROLLER, "$url =", Direction::Backward);
    let structural = [
        "namespace_definition",
        "namespace_name",
        "class_declaration",
        "base_clause",
        "declaration_list",
        "method_declaration",
        "visibility_modifier",
        "formal_parameters",
        "simple_parameter",
        "named_type",
        "qualified_name",
        "scoped_call_expression",
        "arguments",
        "type_list",
    ];
    let unsupported: Vec<&str> = report
        .boundaries
        .iter()
        .filter(|b| b.kind == "unsupported_semantics")
        .filter_map(|b| b.message.split(' ').next())
        .collect();
    assert!(
        unsupported.iter().all(|kind| !structural.contains(kind)),
        "structural nodes must not raise boundaries: {unsupported:?}"
    );
    assert!(
        unsupported.contains(&"object_creation_expression"),
        "a real unsupported construct keeps its boundary: {unsupported:?}"
    );
    assert!(
        unsupported.contains(&"try_statement"),
        "exception paths keep their boundary: {unsupported:?}"
    );
}

#[test]
fn test_analyze_php_anonymous_class_method_is_not_a_free_function() {
    let files = [source(
        "anon.php",
        "<?php\n$o = new class {\n    public function fmt($s) { return $s; }\n};\n$r = fmt(101);\nconsume($r);\n",
    )];
    let report = forward(&files, "anon.php", "101");
    assert!(
        !binding(&report, "anon.php", "$s"),
        "a method name is not bound in the enclosing scope: {report:#?}"
    );
    let call = line_of(&files[0], "fmt(101)");
    assert!(
        report
            .boundaries
            .iter()
            .any(|b| b.kind == "external_call" && b.line == call),
        "{:?}",
        report.boundaries
    );
}

const PAIR: &str = r#"<?php
namespace P;
class One
{
    public static function id($one) { return $one; }
}
class Two
{
    public static function id($two) { return $two; }
}
"#;

#[test]
fn test_analyze_php_alias_resolves_per_namespace_block() {
    let files = [
        source("pair.php", PAIR),
        source(
            "main.php",
            "<?php\nnamespace A;\nuse P\\One as X;\nconsume(X::id(101));\nnamespace B;\nuse P\\Two as X;\nconsume(X::id(202));\n",
        ),
    ];
    let first = forward(&files, "main.php", "101");
    assert!(binding(&first, "pair.php", "$one"), "{first:#?}");
    assert!(
        !binding(&first, "pair.php", "$two"),
        "namespace B's alias must not leak into A: {first:#?}"
    );
    let second = forward(&files, "main.php", "202");
    assert!(binding(&second, "pair.php", "$two"), "{second:#?}");
    assert!(!binding(&second, "pair.php", "$one"), "{second:#?}");
}

#[test]
fn test_analyze_php_same_named_classes_resolve_per_namespace_block() {
    let files = [source(
        "twins.php",
        "<?php\nnamespace A;\nclass Foo\n{\n    public static function id($inA) { return $inA; }\n}\nconsume(Foo::id(101));\nnamespace B;\nclass Foo\n{\n    public static function id($inB) { return $inB; }\n}\n",
    )];
    let report = forward(&files, "twins.php", "101");
    assert!(binding(&report, "twins.php", "$inA"), "{report:#?}");
    assert!(!binding(&report, "twins.php", "$inB"), "{report:#?}");
}

#[test]
fn test_analyze_php_alias_lookup_ignores_case() {
    let files = [
        source(
            "app/Models/User.php",
            "<?php\nnamespace App\\Models;\nclass User\n{\n    public static function make($m) { return $m; }\n}\n",
        ),
        source(
            "main.php",
            "<?php\nnamespace App;\nuse App\\Models\\User as Member;\nconsume(member::make(101));\n",
        ),
    ];
    let report = forward(&files, "main.php", "101");
    assert!(
        binding(&report, "app/Models/User.php", "$m"),
        "PHP class names and aliases are case-insensitive: {report:#?}"
    );
    assert!(argument(&report, "member::make(101)"), "{report:#?}");
}

#[test]
fn test_analyze_php_method_sees_function_declared_after_class() {
    let files = [source(
        "later.php",
        "<?php\nclass Svc\n{\n    public static function run($v) { return later($v); }\n}\nfunction later($x) { return $x; }\nconsume(Svc::run(101));\n",
    )];
    let report = forward(&files, "later.php", "101");
    assert!(binding(&report, "later.php", "$x"), "{report:#?}");
    assert!(argument(&report, "Svc::run(101)"), "{report:#?}");
}
