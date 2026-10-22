//! PHP dynamic method dispatch: `Rows::$m()`, `static::$m()`, `$this->$m()`
//! where `$m` comes from a same-file class constant map of string literals
//! (`$m = self::MAP[$type] ?? null`). The parser enumerates the map's values
//! as a fan-out, so `ecp impact` on `Rows::hero` lists the dispatcher instead
//! of reporting the method dead. Any binding the parser cannot enumerate keeps
//! only the `php-variable-method-call` blind spot.
//!
//! Also covers the nullsafe call `$r?->inst()`, which must bind like
//! `$r->inst()`.

mod instantiation_calls_support;

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawFanoutRef};
use ecp_core::graph::{NodeKind, RelType};
use instantiation_calls_support::{calls_from, edges_from, graph_of};
use std::path::Path;

const REASON: &str = "php-const-map-dispatch";
const BLIND: &str = "php-variable-method-call";

fn parse_php(src: &str) -> LocalGraph {
    PhpProvider::new()
        .expect("PhpProvider::new")
        .parse_file(Path::new("src/Api.php"), src.as_bytes())
        .expect("parse_file")
}

fn fanouts(g: &LocalGraph) -> Vec<&RawFanoutRef> {
    g.fanout_refs
        .iter()
        .filter(|f| f.reason == REASON)
        .collect()
}

fn blind_count(g: &LocalGraph) -> usize {
    g.blind_spots.iter().filter(|b| b.kind == BLIND).count()
}

/// The single fan-out of `src`, from `source`, with exactly `candidates`;
/// the site records no blind spot because the fan-out names every value.
fn assert_fanout(src: &str, source: &str, candidates: &[&str]) {
    let g = parse_php(src);
    let found = fanouts(&g);
    assert_eq!(found.len(), 1, "expected one fan-out: {:?}", g.fanout_refs);
    let fanout = found[0];
    assert_eq!(fanout.source_name, source);
    assert_eq!(fanout.candidates, candidates);
    assert!((fanout.base_confidence - 0.5).abs() < 1e-6);
    assert_eq!(blind_count(&g), 0, "blind spots: {:?}", g.blind_spots);
}

/// The single fan-out of `src`, from `source`, with exactly `candidates`,
/// whose site keeps its blind spot: a late-bound receiver (`$this`,
/// `static`) may dispatch to a subclass declaring the methods elsewhere.
fn assert_fanout_keeps_blind_spot(src: &str, source: &str, candidates: &[&str]) {
    let g = parse_php(src);
    let found = fanouts(&g);
    assert_eq!(found.len(), 1, "expected one fan-out: {:?}", g.fanout_refs);
    assert_eq!(found[0].source_name, source);
    assert_eq!(found[0].candidates, candidates);
    assert_eq!(blind_count(&g), 1, "blind spots: {:?}", g.blind_spots);
}

/// `src` emits no fan-out and keeps exactly one variable-method blind spot.
fn assert_blind_only(src: &str) {
    let g = parse_php(src);
    assert!(
        fanouts(&g).is_empty(),
        "no fan-out expected: {:?}",
        g.fanout_refs
    );
    assert_eq!(blind_count(&g), 1, "blind spots: {:?}", g.blind_spots);
}

/// Wraps `body` as the method `dispatch(string $type, array $p)` of class
/// `Api`, which declares `$consts`.
fn api(consts: &str, body: &str) -> String {
    format!(
        "<?php\nnamespace App;\n\nclass Api\n{{\n    {consts}\n\n    public function dispatch(string $type, array $p)\n    {{\n        {body}\n    }}\n\n    public function a() {{}}\n    public function b() {{}}\n}}\n"
    )
}

// ── positive: the value set is fully enumerable ──

#[test]
fn test_parse_file_self_const_map_with_null_default_emits_fanout() {
    let src = api(
        "private const ROW_METHODS = ['H' => 'hero', 'S' => 'story'];",
        "$map = self::ROW_METHODS[$type] ?? null;\n        if ($map === null) { return null; }\n        return Rows::$map($p);",
    );
    assert_fanout(&src, "dispatch", &["Rows.hero", "Rows.story"]);
}

#[test]
fn test_parse_file_static_const_map_static_receiver_emits_fanout_and_keeps_blind_spot() {
    let src = api(
        "protected const MAP = ['x' => 'a', 'y' => 'b'];",
        "$m = static::MAP[$type];\n        return static::$m($p);",
    );
    assert_fanout_keeps_blind_spot(&src, "dispatch", &["Api.a", "Api.b"]);
}

#[test]
fn test_parse_file_this_receiver_self_const_emits_fanout_and_keeps_blind_spot() {
    let src = api(
        "const MAP = ['a', 'b'];",
        "$m = self::MAP[$type];\n        return $this->$m($p);",
    );
    assert_fanout_keeps_blind_spot(&src, "dispatch", &["Api.a", "Api.b"]);
}

#[test]
fn test_parse_file_self_receiver_emits_fanout_without_blind_spot() {
    let src = api(
        "const MAP = ['a', 'b'];",
        "$m = self::MAP[$type];\n        return self::$m($p);",
    );
    assert_fanout(&src, "dispatch", &["Api.a", "Api.b"]);
}

#[test]
fn test_parse_file_string_literal_key_emits_only_that_value() {
    let src = api(
        "const MAP = ['h' => 'hero', 's' => 'story'];",
        "$m = self::MAP['s'];\n        return Rows::$m($p);",
    );
    assert_fanout(&src, "dispatch", &["Rows.story"]);
}

#[test]
fn test_parse_file_integer_key_on_list_emits_only_that_value() {
    let src = api(
        "const MAP = ['hero', 'story'];",
        "$m = self::MAP[1];\n        return Rows::$m($p);",
    );
    assert_fanout(&src, "dispatch", &["Rows.story"]);
}

#[test]
fn test_parse_file_literal_key_missing_from_map_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['h' => 'hero'];",
        "$m = self::MAP['zzz'];\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_integer_key_on_mixed_array_unkeyed_element_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['h' => 'hero', 'story'];",
        "$m = self::MAP[0];\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_string_offset_write_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$m = self::MAP[$type];\n        $m[0] = 'z';\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_qualified_name_const_records_blind_spot_only() {
    let src = "<?php\nnamespace App;\nclass Rows\n{\n    const MAP = ['hero'];\n}\nclass Api\n{\n    public function dispatch($t)\n    {\n        $m = \\Legacy\\Rows::MAP[$t];\n        return \\Legacy\\Rows::$m();\n    }\n}\n";
    assert_blind_only(src);
}

#[test]
fn test_parse_file_short_name_declared_only_in_other_namespace_emits_ambiguous_fanout() {
    let src = "<?php\nnamespace A {\n    class Rows\n    {\n        const MAP = ['hero'];\n    }\n}\nnamespace B {\n    class Api\n    {\n        public function dispatch($t)\n        {\n            $m = Rows::MAP[$t] ?? null;\n            return Rows::$m();\n        }\n    }\n}\n";
    assert_eq!(ambiguous_fanout(src).candidates, ["Rows.hero"]);
}

#[test]
fn test_parse_file_statement_namespace_same_as_class_keeps_plain_reason() {
    let src = "<?php\nnamespace App;\n\nclass Rows\n{\n    const MAP = ['hero'];\n}\nclass Api\n{\n    public function dispatch($t)\n    {\n        $m = Rows::MAP[$t] ?? null;\n        return Rows::$m();\n    }\n}\n";
    assert_fanout(src, "dispatch", &["Rows.hero"]);
}

#[test]
fn test_parse_file_two_sites_sharing_name_variable_emit_two_fanouts() {
    let src = api(
        "const MAP = ['hero', 'story'];",
        "$m = self::MAP[$type];\n        Rows::$m($p);\n        return Rows::$m($p);",
    );
    let g = parse_php(&src);
    let found = fanouts(&g);
    assert_eq!(found.len(), 2, "{:?}", g.fanout_refs);
    for f in found {
        assert_eq!(f.candidates, ["Rows.hero", "Rows.story"]);
    }
    assert_eq!(blind_count(&g), 0, "blind spots: {:?}", g.blind_spots);
}

#[test]
fn test_parse_file_named_class_const_with_literal_default_emits_fanout() {
    let src = "<?php\nclass Rows\n{\n    const MAP = [ 'hero', ];\n    public static function hero() {}\n    public static function story() {}\n}\nclass Api\n{\n    public function dispatch($type)\n    {\n        $m = Rows::MAP[$type] ?? 'story';\n        return Rows::$m();\n    }\n}\n";
    assert_fanout(src, "dispatch", &["Rows.hero", "Rows.story"]);
}

/// The one fan-out of `src`, which must carry the `-ambiguous` reason.
fn ambiguous_fanout(src: &str) -> RawFanoutRef {
    let g = parse_php(src);
    let found: Vec<_> = g
        .fanout_refs
        .iter()
        .filter(|f| f.reason.starts_with(REASON))
        .collect();
    assert_eq!(found.len(), 1, "expected one fan-out: {:?}", g.fanout_refs);
    assert_eq!(found[0].reason, "php-const-map-dispatch-ambiguous");
    found[0].clone()
}

#[test]
fn test_parse_file_short_name_declared_in_two_namespaces_emits_ambiguous_fanout() {
    let src = "<?php\nnamespace A {\n    class Rows\n    {\n        const MAP = ['hero'];\n    }\n}\nnamespace B {\n    class Rows\n    {\n        const MAP = ['story'];\n    }\n    class Api\n    {\n        public function dispatch($t)\n        {\n            $m = Rows::MAP[$t] ?? null;\n            return Rows::$m();\n        }\n    }\n}\n";
    let fanout = ambiguous_fanout(src);
    assert_eq!(fanout.candidates, ["Rows.hero", "Rows.story"]);
}

#[test]
fn test_parse_file_short_name_also_imported_with_use_emits_ambiguous_fanout() {
    let src = "<?php\nnamespace A {\n    class Rows\n    {\n        const MAP = ['hero'];\n    }\n}\nnamespace B {\n    use Other\\Rows;\n    class Api\n    {\n        public function dispatch($t)\n        {\n            $m = Rows::MAP[$t] ?? null;\n            return Rows::$m();\n        }\n    }\n}\n";
    let fanout = ambiguous_fanout(src);
    assert_eq!(fanout.candidates, ["Rows.hero"]);
}

#[test]
fn test_parse_file_use_function_of_same_short_name_keeps_plain_reason() {
    let src = "<?php\nuse function Other\\Rows;\nclass Rows\n{\n    const MAP = ['hero'];\n    public static function hero() {}\n}\nclass Api\n{\n    public function dispatch($t)\n    {\n        $m = Rows::MAP[$t] ?? null;\n        return Rows::$m();\n    }\n}\n";
    assert_fanout(src, "dispatch", &["Rows.hero"]);
}

#[test]
fn test_parse_file_grouped_use_function_of_same_short_name_keeps_plain_reason() {
    let src = "<?php\nuse Other\\{function Rows};\nclass Rows\n{\n    const MAP = ['hero'];\n    public static function hero() {}\n}\nclass Api\n{\n    public function dispatch($t)\n    {\n        $m = Rows::MAP[$t] ?? null;\n        return Rows::$m();\n    }\n}\n";
    assert_fanout(src, "dispatch", &["Rows.hero"]);
}

#[test]
fn test_parse_file_old_style_array_const_emits_fanout() {
    let src = api(
        "const MAP = array('x' => 'hero', 'y' => \"story\");",
        "$m = self::MAP[$type] ?? NULL;\n        return Rows::$m($p);",
    );
    assert_fanout(&src, "dispatch", &["Rows.hero", "Rows.story"]);
}

#[test]
fn test_parse_file_string_literal_assignments_emit_fanout() {
    let src = api(
        "",
        "if ($type === 'h') { $m = 'hero'; } else { $m = \"story\"; }\n        return Rows::$m($p);",
    );
    assert_fanout(&src, "dispatch", &["Rows.hero", "Rows.story"]);
}

#[test]
fn test_parse_file_duplicate_values_emit_deduplicated_candidates() {
    let src = api(
        "const MAP = ['a' => 'hero', 'b' => 'hero', 'c' => 'story'];",
        "$m = self::MAP[$type] ?? 'hero';\n        return Rows::$m($p);",
    );
    assert_fanout(&src, "dispatch", &["Rows.hero", "Rows.story"]);
}

/// PHP method names are case-insensitive: `Hero` and `hero` name one method,
/// so one candidate stands for both, spelled as first written.
#[test]
fn test_parse_file_case_variant_values_emit_one_candidate() {
    let src = api(
        "const MAP = ['a' => 'hero', 'b' => 'Hero'];",
        "$m = self::MAP[$type];\n        return Rows::$m($p);",
    );
    assert_fanout(&src, "dispatch", &["Rows.hero"]);
}

#[test]
fn test_parse_file_reads_of_name_variable_keep_fanout() {
    let src = api(
        "const MAP = ['hero'];",
        "$m = self::MAP[$type] ?? null;\n        if (!$m || !method_exists(Rows::class, $m)) { return null; }\n        $log = function () use ($m) { return $m; };\n        return Rows::$m($p);",
    );
    assert_fanout(&src, "dispatch", &["Rows.hero"]);
}

// ── negative: the value set is not enumerable → blind spot only ──

#[test]
fn test_parse_file_int_const_value_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero', 1];",
        "$m = self::MAP[$type];\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_nested_array_const_value_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['h' => ['hero']];",
        "$m = self::MAP[$type];\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_empty_const_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = [];",
        "$m = self::MAP[$type] ?? null;\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_param_sourced_name_records_blind_spot_only() {
    assert_blind_only(&api("", "return Rows::$type($p);"));
}

#[test]
fn test_parse_file_param_reassigned_from_const_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$type = self::MAP[$type];\n        return Rows::$type($p);",
    ));
}

#[test]
fn test_parse_file_non_literal_reassignment_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$m = self::MAP[$type] ?? null;\n        $m = strtolower($m);\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_augmented_assignment_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$m = self::MAP[$type];\n        $m .= 'Row';\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_by_reference_closure_use_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$m = self::MAP[$type];\n        $f = function () use (&$m) { $m = 'story'; };\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_foreach_bound_name_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "foreach (self::MAP as $m) { Rows::$m($p); }",
    ));
}

#[test]
fn test_parse_file_list_destructured_name_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$m = self::MAP[$type];\n        [$m] = $p;\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_parent_class_const_records_blind_spot_only() {
    let src = "<?php\nclass Base\n{\n    const MAP = ['hero'];\n}\nclass Api extends Base\n{\n    public function dispatch($type)\n    {\n        $m = self::MAP[$type];\n        return Rows::$m();\n    }\n}\n";
    assert_blind_only(src);
}

#[test]
fn test_parse_file_const_of_class_in_other_file_records_blind_spot_only() {
    assert_blind_only(&api(
        "",
        "$m = Other::MAP[$type];\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_call_inside_closure_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$m = self::MAP[$type];\n        $f = function () use ($m, $p) { return Rows::$m($p); };\n        return $f();",
    ));
}

#[test]
fn test_parse_file_untyped_receiver_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$m = self::MAP[$type];\n        $o = new Rows();\n        return $o->$m($p);",
    ));
}

#[test]
fn test_parse_file_parent_receiver_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$m = self::MAP[$type];\n        return parent::$m($p);",
    ));
}

#[test]
fn test_parse_file_interpolated_string_records_blind_spot_only() {
    assert_blind_only(&api("", "$m = \"h{$type}\";\n        return Rows::$m($p);"));
}

#[test]
fn test_parse_file_heredoc_value_records_blind_spot_only() {
    assert_blind_only(&api(
        "",
        "$m = <<<EOT\nhero\nEOT;\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_qualified_string_value_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['Rows::hero'];",
        "$m = self::MAP[$type];\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_whitespace_string_value_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = [' hero', ' '];",
        "$m = self::MAP[$type];\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_whole_const_without_subscript_records_blind_spot_only() {
    assert_blind_only(&api(
        "const MAP = ['hero'];",
        "$m = self::MAP;\n        return Rows::$m($p);",
    ));
}

#[test]
fn test_parse_file_name_never_assigned_records_blind_spot_only() {
    assert_blind_only(&api("", "return Rows::$m($p);"));
}

// ── graph: the candidates resolve to the methods ──

const ROWS: &str = "<?php\nnamespace App;\n\nfinal class Rows\n{\n    public static function hero(array $p): array { return $p; }\n    public static function story(array $p): array { return $p; }\n    public function inst(): int { return 1; }\n}\n";

const API: &str = "<?php\nnamespace App;\n\nclass Api\n{\n    protected const MAP = ['H' => 'hero', 'S' => 'story'];\n\n    public function dispatch(string $type, array $p): ?array\n    {\n        $m = self::MAP[$type] ?? null;\n        return Rows::$m($p);\n    }\n\n    public function viaNullsafe(?Rows $r): ?int\n    {\n        return $r?->inst();\n    }\n\n    public function plain(Rows $r): int\n    {\n        return $r->inst();\n    }\n}\n";

/// `(file, owner, name)` of every `References` edge from `caller` to a
/// method, sorted, and the confidence of each. A `self::MAP` read references
/// the constant too; only the method edges are the fan-out's.
fn references(
    graph: &ecp_core::graph::ZeroCopyGraph,
    caller: &str,
) -> (Vec<(String, String, String)>, Vec<f32>) {
    let hits: Vec<_> = edges_from(graph, caller, RelType::References)
        .into_iter()
        .filter(|h| h.kind == NodeKind::Method)
        .collect();
    let confidences = hits.iter().map(|h| h.confidence).collect();
    let mut targets: Vec<_> = hits
        .into_iter()
        .map(|h| (h.file, h.owner, h.name))
        .collect();
    targets.sort();
    (targets, confidences)
}

fn target(file: &str, owner: &str, name: &str) -> (String, String, String) {
    (file.to_string(), owner.to_string(), name.to_string())
}

#[test]
fn test_build_const_map_dispatch_resolves_static_methods_in_other_file() {
    let graph = graph_of(
        &PhpProvider::new().unwrap(),
        &[("src/Rows.php", ROWS), ("src/Api.php", API)],
    );
    let (targets, confidences) = references(&graph, "dispatch");
    assert_eq!(
        targets,
        vec![
            target("src/Rows.php", "Rows", "hero"),
            target("src/Rows.php", "Rows", "story"),
        ]
    );
    for c in confidences {
        assert!(
            (c - 0.5 / 2f32.sqrt()).abs() < 1e-4 && c < 0.7,
            "fan-out confidence {c}"
        );
    }
}

#[test]
fn test_build_const_map_dispatch_through_use_import_resolves_other_namespace() {
    let rows = "<?php\nnamespace App\\Rows;\n\nfinal class ProductRows\n{\n    public static function hero(array $p): array { return $p; }\n    public static function faq(array $p): array { return $p; }\n}\n";
    let api = "<?php\nnamespace App\\Http;\n\nuse App\\Rows\\ProductRows;\n\nclass ContentApi\n{\n    private const ROW_METHODS = ['hero' => 'hero', 'faq' => 'faq'];\n\n    public function rows(string $type, array $payload): ?array\n    {\n        $map = self::ROW_METHODS[$type] ?? null;\n        if ($map === null) {\n            return null;\n        }\n        return ProductRows::$map($payload);\n    }\n}\n";
    let graph = graph_of(
        &PhpProvider::new().unwrap(),
        &[
            ("app/Rows/ProductRows.php", rows),
            ("app/Http/ContentApi.php", api),
        ],
    );
    let (targets, _) = references(&graph, "rows");
    assert_eq!(
        targets,
        vec![
            target("app/Rows/ProductRows.php", "ProductRows", "faq"),
            target("app/Rows/ProductRows.php", "ProductRows", "hero"),
        ]
    );
}

#[test]
fn test_build_nullsafe_member_call_binds_like_member_call() {
    let graph = graph_of(
        &PhpProvider::new().unwrap(),
        &[("src/Rows.php", ROWS), ("src/Api.php", API)],
    );
    let bound = |caller: &str| -> Vec<(String, String, String)> {
        calls_from(&graph, caller)
            .into_iter()
            .map(|h| (h.file, h.owner, h.name))
            .collect()
    };
    let plain = bound("plain");
    assert_eq!(plain, vec![target("src/Rows.php", "Rows", "inst")]);
    assert_eq!(bound("viaNullsafe"), plain);
}

#[test]
fn test_parse_file_nullsafe_call_in_arrow_callback_hosts_anonymous_node() {
    let src = "<?php\nclass Api\n{\n    public function all(array $rows)\n    {\n        return array_map(fn($r) => $r?->inst(), $rows);\n    }\n}\n";
    let g = parse_php(src);
    let hosts: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("<anonymous") && n.calls.iter().any(|c| c.contains("inst")))
        .collect();
    assert_eq!(hosts.len(), 1, "nodes: {:?}", g.nodes);
}

/// `(kind, owner)` of every `References` source reaching the callable
/// `target` (a Python `def` is a `Function`).
fn reference_sources(
    graph: &ecp_core::graph::ZeroCopyGraph,
    target: &str,
) -> Vec<(NodeKind, String)> {
    let pool = graph.string_pool.as_slice();
    let mut sources: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::References)
        .filter(|e| {
            let t = &graph.nodes[e.target as usize];
            matches!(t.kind, NodeKind::Method | NodeKind::Function)
                && t.name.resolve(pool) == target
        })
        .map(|e| {
            let s = &graph.nodes[e.source as usize];
            (s.kind, s.owner_class.resolve(pool).to_string())
        })
        .collect();
    sources.sort_by(|a, b| a.1.cmp(&b.1));
    sources
}

#[test]
fn test_build_fanout_source_is_method_not_same_named_property() {
    let src = "<?php\nnamespace App;\nclass Rows\n{\n    public static function hero() {}\n}\nclass Api\n{\n    protected const MAP = ['hero'];\n    private array $render = [];\n\n    public function render($t)\n    {\n        $m = self::MAP[$t] ?? null;\n        return Rows::$m();\n    }\n}\n";
    let graph = graph_of(&PhpProvider::new().unwrap(), &[("src/Api.php", src)]);
    assert_eq!(
        reference_sources(&graph, "hero"),
        [(NodeKind::Method, "Api".to_string())]
    );
}

#[test]
fn test_build_fanout_source_is_enclosing_class_method_not_first_same_named() {
    let src = "<?php\nnamespace App;\nclass Rows\n{\n    public static function hero() {}\n}\nclass Plain\n{\n    public function handle() { return 1; }\n}\nclass Api\n{\n    const MAP = ['hero'];\n\n    public function handle($t)\n    {\n        $m = self::MAP[$t];\n        return Rows::$m();\n    }\n}\n";
    let graph = graph_of(&PhpProvider::new().unwrap(), &[("src/Api.php", src)]);
    assert_eq!(
        reference_sources(&graph, "hero"),
        [(NodeKind::Method, "Api".to_string())]
    );
}

#[test]
fn test_build_python_getattr_fanout_source_is_enclosing_class_method() {
    let src = "class Plain:\n    def dispatch(self, action):\n        return 1\n\n\nclass Handler:\n    def dispatch(self, action):\n        name = f\"handle_{action}\"\n        return getattr(self, name)()\n\n    def handle_create(self):\n        return 1\n";
    let graph = graph_of(
        &ecp_analyzer::python::PythonProvider::new().unwrap(),
        &[("disp.py", src)],
    );
    let sources = reference_sources(&graph, "handle_create");
    assert!(!sources.is_empty(), "no fan-out edge reached handle_create");
    assert!(
        sources.iter().all(|(_, owner)| owner == "Handler"),
        "{sources:?}"
    );
}
