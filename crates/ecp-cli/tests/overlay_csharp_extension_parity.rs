//! A C# extension-method call out of a dirty file (codex ruling P-ext): the
//! session overlay binds the same extension edge, at the same confidence
//! and reason, as a full reindex does. The overlay reads the extension flag
//! from the base graph's `FunctionMeta` for a clean file and from the fresh
//! parse for a dirty one; a dirty fragment with no flags (an older writer's)
//! makes the overlay bind nothing (the documented fallback), never a wrong
//! edge.
//!
//! P-ext2 B: every fallback is observable. Where the overlay cannot match
//! the reindex, it reports a [`CallGap`] naming the call site and a reason
//! code, which reaches the payload's `result` caveat.

use crate::parity;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::csharp_extension::{HEURISTIC_CONFIDENCE, HEURISTIC_REASON};
use ecp_core::graph::RelType;
use ecp_core::session::view::{CallGap, GAP_CS_EXTENSION_DIRTY_DECLARATION};
use parity::{
    call_gaps_after_edit, call_gaps_after_edit_with, callees, calls_after_edit_in,
    edges_after_edit, named, provider_parse,
};

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
    let gaps = call_gaps_after_edit(
        &provider_parse(&provider),
        &[WIDGET, ("Lib/WidgetExt.cs", EXT), ("App.cs", APP)],
        &[("App.cs", &app)],
    );
    assert_eq!(gaps, vec![], "parity reports no fallback");
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

/// Stands for: a dirty extension file's parse carries its flags, so the
/// overlay binds the `Spin` the reindex binds, and reports no gap.
#[test]
fn test_overlay_dirty_extension_declaration_matches_reindex() {
    let provider = CSharpProvider::new().unwrap();
    let ext = EXT.replacen("    {\n", "    {\n\n", 1);
    let files = [WIDGET, ("Lib/WidgetExt.cs", EXT), ("App.cs", APP)];
    let app = edited_app();
    let edits = [("Lib/WidgetExt.cs", ext.as_str()), ("App.cs", app.as_str())];
    let parity = calls_after_edit_in(&provider, &files, &edits, &named("Run"));
    let spins = |edges: &[parity::CallEdge]| {
        edges
            .iter()
            .filter(|e| e.callee == "Spin")
            .map(|e| (e.callee_line, e.confidence))
            .collect::<Vec<_>>()
    };
    assert_eq!(spins(&parity.reindex), vec![(6, 0.85)]);
    assert_eq!(parity.overlay, parity.reindex);
    let gaps = call_gaps_after_edit(&provider_parse(&provider), &files, &edits);
    assert_eq!(gaps, vec![]);
}

/// A fragment an older writer wrote carries no flags.
fn without_flags(input: &mut ecp_core::session::view::OverlayFileInput) {
    for symbol in &mut input.symbols {
        symbol.flags = None;
    }
}

/// Contract of the fallback: a dirty extension file whose fragment carries
/// no flags binds no `Spin`, and the missing edge is reported once per call
/// site, not silent (P-ext2 B): `Run` calls `Spin` on `w` and on `Make()`.
#[test]
fn test_overlay_dirty_extension_declaration_without_flags_reports_one_gap() {
    let provider = CSharpProvider::new().unwrap();
    let ext = EXT.replacen("    {\n", "    {\n\n", 1);
    let app = edited_app();
    let gaps = call_gaps_after_edit_with(
        &provider_parse(&provider),
        &[WIDGET, ("Lib/WidgetExt.cs", EXT), ("App.cs", APP)],
        &[("Lib/WidgetExt.cs", &ext), ("App.cs", &app)],
        &without_flags,
    );
    let want = gap("App.cs", 8, GAP_CS_EXTENSION_DIRTY_DECLARATION);
    assert_eq!(gaps.iter().filter(|g| **g == want).count(), 1, "{gaps:?}");
}

/// `Run` (line 7 in `APP`, 8 after `edited_app`) calls `Spin` with `reason`.
fn gap(path: &str, line: u32, reason: &str) -> CallGap {
    CallGap {
        path: path.to_string(),
        line,
        caller: "Run".to_string(),
        member: "Spin".to_string(),
        reason: reason.to_string(),
    }
}

/// P-ext2 A on the overlay: a dirty caller's arguments pick the same
/// overload the reindex picks.
#[test]
fn test_overlay_csharp_extension_overloads_by_arity_match_reindex() {
    let provider = CSharpProvider::new().unwrap();
    let ext = EXT.replace(
        "        public static T Echo",
        "        public static int Spin(this Widget w, int times) { return times; }\n        public static T Echo",
    );
    let app = edited_app().replace("w.Spin() + Make().Spin()", "w.Spin(3) + Make().Spin(4)");
    let parity = calls_after_edit_in(
        &provider,
        &[WIDGET, ("Lib/WidgetExt.cs", &ext), ("App.cs", APP)],
        &[("App.cs", &app)],
        &named("Run"),
    );
    let spins: Vec<_> = callees(&parity.reindex)
        .into_iter()
        .filter(|callee| callee.1 == "Spin")
        .collect();
    assert_eq!(spins, vec![("Lib/WidgetExt.cs", "Spin", 6, 0.85)]);
    assert_eq!(parity.overlay, parity.reindex);
}

/// The three P-ext2 B edits to a dirty file, with the caller `App.cs`
/// clean. The invalidation closure re-resolves `Run`, the clean caller, so
/// each overlay matches the reindex and reports nothing.
fn assert_clean_caller_matches(edit: (&str, &str), extra: &[(&str, &str)]) {
    let provider = CSharpProvider::new().unwrap();
    let mut files = vec![WIDGET, ("Lib/WidgetExt.cs", EXT), ("App.cs", APP)];
    files.extend_from_slice(extra);
    let parity = calls_after_edit_in(&provider, &files, &[edit], &named("Run"));
    let gaps = call_gaps_after_edit(&provider_parse(&provider), &files, &[edit]);
    assert_eq!(parity.overlay, parity.reindex, "{gaps:?}");
    assert_eq!(gaps, vec![]);
}

/// Edit 1: the extension now extends another type.
#[test]
fn test_overlay_dirty_extension_retargeted_matches_reindex() {
    let ext = EXT.replace("Spin(this Widget w)", "Spin(this Gadget w)");
    assert_clean_caller_matches(
        ("Lib/WidgetExt.cs", &ext),
        &[("Gadget.cs", "public class Gadget\n{\n}\n")],
    );
}

/// Edit 2: the receiver type gains a plain instance method of that name,
/// which wins over the extension in a full build.
#[test]
fn test_overlay_dirty_instance_namesake_matches_reindex() {
    let widget = "public class Widget\n{\n    public int Spin() { return 0; }\n}\n";
    assert_clean_caller_matches(("Widget.cs", widget), &[]);
}

/// Edit 3: the declaration loses `this` and is no extension any more.
#[test]
fn test_overlay_dirty_extension_loses_this_matches_reindex() {
    let ext = EXT.replace("Spin(this Widget w)", "Spin(Widget w)");
    assert_clean_caller_matches(("Lib/WidgetExt.cs", &ext), &[]);
}

/// Stands for: `string.Wrap(..)` calls a static member of the predefined
/// type `string`, not a method on a string value, so no extension of
/// `string` binds it, on either side. Before the fix the overlay retried
/// the extension with `string` as a named receiver.
#[test]
fn test_overlay_keyword_type_receiver_binds_no_extension() {
    let provider = CSharpProvider::new().unwrap();
    let ext = "namespace Lib\n{\n    public static class StrExt\n    {\n        public static string Wrap(this string s, string t) { return s + t; }\n    }\n}\n";
    let app = "namespace Lib\n{\n    public class Runner\n    {\n        public void Run() { string.Wrap(\"a\"); }\n    }\n}\n";
    let edited = app.replacen("    {\n", "    {\n\n", 1);
    let files = [("Lib/StrExt.cs", ext), ("Lib/Runner.cs", app)];
    let edits = [("Lib/Runner.cs", edited.as_str())];
    let parity = calls_after_edit_in(&provider, &files, &edits, &named("Run"));
    let wraps = |edges: &[parity::CallEdge]| edges.iter().filter(|e| e.callee == "Wrap").count();
    assert_eq!(wraps(&parity.reindex), 0);
    assert_eq!(parity.overlay, parity.reindex);
}

/// Guard: a dirty C# file whose methods name no extension reports nothing.
#[test]
fn test_overlay_dirty_unrelated_method_reports_no_gap() {
    let provider = CSharpProvider::new().unwrap();
    let widget = "public class Widget\n{\n    public int Turn() { return 0; }\n}\n";
    let gaps = call_gaps_after_edit(
        &provider_parse(&provider),
        &[WIDGET, ("Lib/WidgetExt.cs", EXT), ("App.cs", APP)],
        &[("Widget.cs", widget)],
    );
    assert_eq!(gaps, vec![]);
}
