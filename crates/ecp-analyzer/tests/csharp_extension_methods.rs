//! C# extension-method calls (codex ruling P-ext): a member call whose
//! receiver type owns no such member retries the extension methods in the
//! caller's scope. An edge binds only for one applicable declaring class;
//! a retry that binds nothing leaves a `cs-extension-unresolved` BlindSpot
//! naming the receiver type and the member, never a same-name global.
//!
//! Every fixture types the receiver as a parameter (`Widget w`), so the
//! parser writes `Widget.Spin`; `Widget` and `Gadget` declare no bases, so
//! their ancestry is closed and a `this Widget` match is proven (0.85, the
//! owned-member tier). Contract strings: `call` is the index's plain call
//! reason, `call:extension-heuristic` the reason of an unproven match.

mod receiver_typing_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::csharp_extension::{
    BS_KIND_UNRESOLVED, HEURISTIC_CONFIDENCE, HEURISTIC_REASON,
};
use ecp_core::graph::{RelType, ZeroCopyGraph};
use receiver_typing_support::{build, parse_all};

const WIDGET: (&str, &str) = ("Widget.cs", "public class Widget\n{\n}\n");
const GADGET: (&str, &str) = ("Gadget.cs", "public class Gadget\n{\n}\n");

/// `Runner.Run(Widget w) => w.Spin()` in namespace `App`, with `usings`.
fn caller(usings: &str) -> String {
    format!(
        "{usings}\nnamespace App\n{{\n    public class Runner\n    {{\n        public int Run(Widget w)\n        {{\n            return w.Spin();\n        }}\n    }}\n}}\n"
    )
}

/// A static class `class` in namespace `ns` declaring `body` (one member per
/// line, the first on line 5).
fn ext_class(ns: &str, class: &str, body: &str) -> String {
    format!("namespace {ns}\n{{\n    public static class {class}\n    {{\n{body}    }}\n}}\n")
}

fn graph_of(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    build(parse_all(&provider, files))
}

/// Every `Calls` edge out of `Run`: (target file, target name, 1-based
/// target line, confidence, reason).
fn run_calls(graph: &ZeroCopyGraph) -> Vec<(String, String, u32, f32, String)> {
    let pool = graph.string_pool.as_slice();
    let mut calls: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "Run")
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            (
                graph.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string(),
                target.name.resolve(pool).to_string(),
                target.span.0 + 1,
                e.confidence,
                e.reason.resolve(pool).to_string(),
            )
        })
        .collect();
    calls.sort_by(|a, b| a.partial_cmp(b).unwrap());
    calls
}

/// The extension BlindSpots: (file, hint).
fn extension_misses(graph: &ZeroCopyGraph) -> Vec<(String, String)> {
    let pool = graph.string_pool.as_slice();
    graph
        .blind_spots
        .iter()
        .filter(|b| b.kind.resolve(pool) == BS_KIND_UNRESOLVED)
        .map(|b| {
            (
                b.file_path.resolve(pool).to_string(),
                b.hint.resolve(pool).to_string(),
            )
        })
        .collect()
}

fn edge(
    file: &str,
    name: &str,
    line: u32,
    confidence: f32,
    reason: &str,
) -> (String, String, u32, f32, String) {
    (file.into(), name.into(), line, confidence, reason.into())
}

const SPIN_WIDGET: &str = "        public static int Spin(this Widget w) { return 1; }\n";

#[test]
fn test_resolve_call_exact_project_receiver_binds_extension_at_owned_tier() {
    let ext = ext_class("Lib", "WidgetExt", SPIN_WIDGET);
    let app = caller("using Lib;");
    let graph = graph_of(&[WIDGET, ("Lib/WidgetExt.cs", &ext), ("App.cs", &app)]);
    assert_eq!(
        run_calls(&graph),
        vec![edge("Lib/WidgetExt.cs", "Spin", 5, 0.85, "call")]
    );
    assert_eq!(extension_misses(&graph), vec![]);
}

#[test]
fn test_resolve_call_generic_this_extension_binds_heuristic() {
    let ext = ext_class(
        "Lib",
        "Fluent",
        "        public static T Spin<T>(this T value) { return value; }\n",
    );
    let app = caller("using Lib;");
    let graph = graph_of(&[WIDGET, ("Lib/Fluent.cs", &ext), ("App.cs", &app)]);
    assert_eq!(
        run_calls(&graph),
        vec![edge(
            "Lib/Fluent.cs",
            "Spin",
            5,
            HEURISTIC_CONFIDENCE,
            HEURISTIC_REASON
        )]
    );
    assert!(
        HEURISTIC_CONFIDENCE < 0.7,
        "a heuristic edge stays under the promotion gate"
    );
}

/// Contract: the call's arguments are not parsed, so one class's overloads
/// are one candidate, bound once at the class's first declaration. Two
/// edges, or a BlindSpot, would mean the overloads split into rivals.
#[test]
fn test_resolve_call_extension_overloads_by_arity_bind_one_edge_to_first() {
    let ext = ext_class(
        "Lib",
        "WidgetExt",
        "        public static int Spin(this Widget w) { return 1; }\n        public static int Spin(this Widget w, int times) { return times; }\n",
    );
    let app = caller("using Lib;").replace("w.Spin()", "w.Spin(3)");
    let graph = graph_of(&[WIDGET, ("Lib/WidgetExt.cs", &ext), ("App.cs", &app)]);
    assert_eq!(
        run_calls(&graph),
        vec![edge("Lib/WidgetExt.cs", "Spin", 5, 0.85, "call")]
    );
    assert_eq!(extension_misses(&graph), vec![]);
}

#[test]
fn test_resolve_call_extension_wrong_receiver_type_excluded() {
    let widget_ext = ext_class("Lib", "WidgetExt", SPIN_WIDGET);
    let gadget_ext = ext_class(
        "Lib",
        "GadgetExt",
        "        public static int Spin(this Gadget g) { return 2; }\n",
    );
    let app = caller("using Lib;");
    let graph = graph_of(&[
        WIDGET,
        GADGET,
        ("Lib/GadgetExt.cs", &gadget_ext),
        ("Lib/WidgetExt.cs", &widget_ext),
        ("App.cs", &app),
    ]);
    assert_eq!(
        run_calls(&graph),
        vec![edge("Lib/WidgetExt.cs", "Spin", 5, 0.85, "call")]
    );
}

#[test]
fn test_resolve_call_extension_namespace_not_in_scope_excluded() {
    let in_scope = ext_class("Lib", "WidgetExt", SPIN_WIDGET);
    let out_of_scope = ext_class("Other", "WidgetExt2", SPIN_WIDGET);
    let app = caller("using Lib;");
    let graph = graph_of(&[
        WIDGET,
        ("Other/WidgetExt2.cs", &out_of_scope),
        ("Lib/WidgetExt.cs", &in_scope),
        ("App.cs", &app),
    ]);
    assert_eq!(
        run_calls(&graph),
        vec![edge("Lib/WidgetExt.cs", "Spin", 5, 0.85, "call")]
    );
}

/// The caller's enclosing namespace is in scope without a `using`.
#[test]
fn test_resolve_call_extension_in_enclosing_namespace_binds() {
    let ext = ext_class("App", "WidgetExt", SPIN_WIDGET);
    let app = caller("").replace("namespace App", "namespace App.Feature");
    let graph = graph_of(&[WIDGET, ("App/WidgetExt.cs", &ext), ("App.cs", &app)]);
    assert_eq!(
        run_calls(&graph),
        vec![edge("App/WidgetExt.cs", "Spin", 5, 0.85, "call")]
    );
}

#[test]
fn test_resolve_call_instance_member_wins_over_extension() {
    let ext = ext_class("Lib", "WidgetExt", SPIN_WIDGET);
    let app = caller("using Lib;");
    let base = (
        "Base.cs",
        "public class Base\n{\n    public int Spin() { return 0; }\n}\n",
    );
    let widget = ("Widget.cs", "public class Widget : Base\n{\n}\n");
    let graph = graph_of(&[base, widget, ("Lib/WidgetExt.cs", &ext), ("App.cs", &app)]);
    let calls = run_calls(&graph);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        (calls[0].0.as_str(), calls[0].2),
        ("Base.cs", 3),
        "{calls:?}"
    );
    assert_eq!(extension_misses(&graph), vec![]);
}

#[test]
fn test_resolve_call_two_applicable_extensions_give_blind_spot_and_no_edge() {
    let a = ext_class("Lib", "AExt", SPIN_WIDGET);
    let b = ext_class("Lib", "BExt", SPIN_WIDGET);
    let app = caller("using Lib;");
    let graph = graph_of(&[
        WIDGET,
        ("Lib/AExt.cs", &a),
        ("Lib/BExt.cs", &b),
        ("App.cs", &app),
    ]);
    assert_eq!(run_calls(&graph), vec![]);
    let misses = extension_misses(&graph);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!(misses[0].0, "App.cs");
    assert!(
        misses[0].1.contains("`Widget` has no member `Spin`"),
        "{misses:?}"
    );
    assert!(misses[0].1.contains("2 classes"), "{misses:?}");
}

#[test]
fn test_resolve_call_failed_extension_lookup_records_blind_spot() {
    let gadget_ext = ext_class(
        "Lib",
        "GadgetExt",
        "        public static int Spin(this Gadget g) { return 2; }\n",
    );
    let app = caller("using Lib;");
    let graph = graph_of(&[
        WIDGET,
        GADGET,
        ("Lib/GadgetExt.cs", &gadget_ext),
        ("App.cs", &app),
    ]);
    assert_eq!(run_calls(&graph), vec![], "never the same-name global");
    let misses = extension_misses(&graph);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert_eq!(misses[0].0, "App.cs");
    assert!(
        misses[0].1.contains("`Widget` has no member `Spin`"),
        "{misses:?}"
    );
    assert!(
        misses[0].1.contains("accepts a `Widget` receiver"),
        "{misses:?}"
    );
}

#[test]
fn test_resolve_call_out_of_scope_extension_records_blind_spot() {
    let ext = ext_class("Other", "WidgetExt", SPIN_WIDGET);
    let app = caller("using Lib;");
    let graph = graph_of(&[WIDGET, ("Other/WidgetExt.cs", &ext), ("App.cs", &app)]);
    assert_eq!(run_calls(&graph), vec![]);
    let misses = extension_misses(&graph);
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert!(misses[0].1.contains("`using`s"), "{misses:?}");
    assert!(
        misses[0].1.contains("`global using` of another file"),
        "{misses:?}"
    );
}

/// A static method without `this` is no extension: no edge and no
/// BlindSpot, as before the retry.
#[test]
fn test_resolve_call_plain_static_namesake_not_bound() {
    let util = ext_class(
        "Lib",
        "Util",
        "        public static int Spin(Widget w) { return 1; }\n",
    );
    let app = caller("using Lib;");
    let graph = graph_of(&[WIDGET, ("Lib/Util.cs", &util), ("App.cs", &app)]);
    assert_eq!(run_calls(&graph), vec![]);
    assert_eq!(extension_misses(&graph), vec![]);
}

#[test]
fn test_resolve_call_private_extension_invisible_outside_its_class() {
    let ext = ext_class(
        "Lib",
        "WidgetExt",
        "        private static int Spin(this Widget w) { return 1; }\n",
    );
    let app = caller("using Lib;");
    let graph = graph_of(&[WIDGET, ("Lib/WidgetExt.cs", &ext), ("App.cs", &app)]);
    assert_eq!(run_calls(&graph), vec![]);
    assert_eq!(extension_misses(&graph).len(), 1);
}

/// A library receiver (`Stream`, no project type) may own `Spin` through a
/// base the index cannot see: the exact `this Stream` match stays heuristic.
#[test]
fn test_resolve_call_library_receiver_extension_binds_heuristic() {
    let ext = ext_class(
        "Lib",
        "StreamExt",
        "        public static int Spin(this Stream s) { return 1; }\n",
    );
    let app = caller("using Lib;").replace("Widget w", "Stream w");
    let graph = graph_of(&[("Lib/StreamExt.cs", &ext), ("App.cs", &app)]);
    assert_eq!(
        run_calls(&graph),
        vec![edge(
            "Lib/StreamExt.cs",
            "Spin",
            5,
            HEURISTIC_CONFIDENCE,
            HEURISTIC_REASON
        )]
    );
}

/// `Make().Spin()`: the receiver is the type `Make` returns.
#[test]
fn test_resolve_chain_returned_receiver_binds_extension() {
    let ext = ext_class("Lib", "WidgetExt", SPIN_WIDGET);
    let app = "using Lib;\nnamespace App\n{\n    public class Runner\n    {\n        public Widget Make() { return new Widget(); }\n        public int Run()\n        {\n            return Make().Spin();\n        }\n    }\n}\n";
    let graph = graph_of(&[WIDGET, ("Lib/WidgetExt.cs", &ext), ("App.cs", app)]);
    let spins: Vec<_> = run_calls(&graph)
        .into_iter()
        .filter(|call| call.1 == "Spin")
        .collect();
    assert_eq!(
        spins,
        vec![edge("Lib/WidgetExt.cs", "Spin", 5, 0.85, "call")]
    );
}
