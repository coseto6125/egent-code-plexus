//! A C# extension-method call out of a dirty file (codex ruling P-ext): the
//! session overlay binds the same extension edge, at the same confidence
//! and reason, as a full reindex does. The overlay reads the extension flag
//! from the base graph's `FunctionMeta`; a dirty file's parse carries no
//! flags, so a dirty file that declares a method of the call's name makes
//! the overlay bind nothing (the documented fallback), never a wrong edge.

#[path = "common/parity.rs"]
mod parity;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::csharp_extension::{HEURISTIC_CONFIDENCE, HEURISTIC_REASON};
use ecp_core::graph::RelType;
use parity::{callees, calls_after_edit_in, edges_after_edit, named, provider_parse};

const WIDGET: (&str, &str) = ("Widget.cs", "public class Widget\n{\n}\n");

const EXT: &str = "namespace Lib\n{\n    public static class WidgetExt\n    {\n        public static int Spin(this Widget w) { return 1; }\n        public static T Echo<T>(this T value) { return value; }\n    }\n}\n";

const APP: &str = "using Lib;\nnamespace App\n{\n    public class Runner\n    {\n        public Widget Make() { return new Widget(); }\n        public int Run(Widget w)\n        {\n            return w.Spin() + Make().Spin();\n        }\n    }\n}\n";

/// The edit moves `Runner`'s members down one line.
fn edited_app() -> String {
    APP.replacen("    {\n", "    {\n\n", 1)
}

#[test]
fn test_overlay_csharp_extension_call_matches_reindex() {
    let provider = CSharpProvider::new().unwrap();
    let app = edited_app();
    let parity = calls_after_edit_in(
        &provider,
        &[WIDGET, ("Lib/WidgetExt.cs", EXT), ("App.cs", APP)],
        &[("App.cs", &app)],
        &named("Run"),
    );
    let spins: Vec<_> = callees(&parity.reindex)
        .into_iter()
        .filter(|callee| callee.1 == "Spin")
        .collect();
    assert_eq!(spins, vec![("Lib/WidgetExt.cs", "Spin", 5, 0.85)]);
    assert_eq!(parity.overlay, parity.reindex);
}

#[test]
fn test_overlay_csharp_generic_extension_keeps_heuristic_reason() {
    let provider = CSharpProvider::new().unwrap();
    let app = edited_app().replace("w.Spin()", "(w.Echo() == null ? 0 : 1)");
    let parity = edges_after_edit(
        &provider_parse(&provider),
        &[WIDGET, ("Lib/WidgetExt.cs", EXT), ("App.cs", APP)],
        &[("App.cs", &app)],
        RelType::Calls,
        &named("Run"),
    );
    let echo: Vec<_> = parity
        .reindex
        .iter()
        .filter(|edge| edge.edge.callee == "Echo")
        .map(|edge| (edge.edge.confidence, edge.reason.as_str()))
        .collect();
    assert_eq!(echo, vec![(HEURISTIC_CONFIDENCE, HEURISTIC_REASON)]);
    let overlay_echo: Vec<_> = parity
        .overlay
        .iter()
        .filter(|edge| edge.edge.callee == "Echo")
        .collect();
    let reindex_echo: Vec<_> = parity
        .reindex
        .iter()
        .filter(|edge| edge.edge.callee == "Echo")
        .collect();
    assert_eq!(overlay_echo, reindex_echo);
}

/// Two applicable classes: neither side binds.
#[test]
fn test_overlay_csharp_ambiguous_extension_binds_nothing_on_both_sides() {
    let provider = CSharpProvider::new().unwrap();
    let rival = EXT.replace("WidgetExt", "WidgetExt2");
    let app = edited_app();
    let parity = calls_after_edit_in(
        &provider,
        &[
            WIDGET,
            ("Lib/WidgetExt.cs", EXT),
            ("Lib/WidgetExt2.cs", &rival),
            ("App.cs", APP),
        ],
        &[("App.cs", &app)],
        &named("Run"),
    );
    let spins = |edges: &[parity::CallEdge]| edges.iter().filter(|e| e.callee == "Spin").count();
    assert_eq!(spins(&parity.reindex), 0);
    assert_eq!(parity.overlay, parity.reindex);
}

/// Contract of the fallback: the dirty extension file's parse carries no
/// flags, so the overlay binds no `Spin` where the reindex binds one. A
/// change that teaches the overlay input the flag turns this into parity.
#[test]
fn test_overlay_dirty_extension_declaration_falls_back_to_no_edge() {
    let provider = CSharpProvider::new().unwrap();
    let ext = EXT.replacen("    {\n", "    {\n\n", 1);
    let parity = calls_after_edit_in(
        &provider,
        &[WIDGET, ("Lib/WidgetExt.cs", EXT), ("App.cs", APP)],
        &[("Lib/WidgetExt.cs", &ext), ("App.cs", &edited_app())],
        &named("Run"),
    );
    let spins = |edges: &[parity::CallEdge]| {
        edges
            .iter()
            .filter(|e| e.callee == "Spin")
            .map(|e| (e.callee_line, e.confidence))
            .collect::<Vec<_>>()
    };
    assert_eq!(spins(&parity.reindex), vec![(6, 0.85)]);
    assert_eq!(spins(&parity.overlay), vec![]);
}
