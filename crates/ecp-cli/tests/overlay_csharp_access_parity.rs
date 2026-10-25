//! The C# access rule on the session overlay (codex ruling P-ext2 C): a
//! dirty caller binds the same calls as a full reindex. An instance call
//! never binds a static method, and a private member binds only from its
//! own type and the types nested in it. The fixture is the Newtonsoft.Json
//! shape of `crates/ecp-analyzer/tests/csharp_call_access.rs`.
//!
//! Codex ruling P-recv: a call on a value receiver binds the same on both
//! sides (`crates/ecp-analyzer/tests/csharp_value_receiver.rs`). A value
//! of unknown type binds no member by its name; only an extension method
//! may bind it, at the same confidence and reason.

#[path = "common/parity.rs"]
mod parity;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::csharp_extension::{HEURISTIC_CONFIDENCE, HEURISTIC_REASON};
use ecp_core::graph::RelType;
use ecp_core::session::view::{
    CallGap, GAP_CS_ACCESS_DIRTY_TARGET, GAP_CS_EXTENSION_DIRTY_DECLARATION,
};
use parity::{
    call_gaps_after_edit, calls_after_edit_in, edges_after_edit, provider_parse, CallEdge,
};

const JTOKEN: (&str, &str) = (
    "Linq/JToken.cs",
    "namespace Lib.Linq
{
    public class JToken
    {
        private static string GetType(JToken token) { return \"\"; }
        public string Explicit(JToken t)
        {
            return JToken.GetType(t);
        }
        public bool Instance(JToken t)
        {
            return t.GetType() == null;
        }
    }
}
",
);

const JCONTAINER: (&str, &str) = (
    "Linq/JContainer.cs",
    "namespace Lib.Linq
{
    public class JContainer : JToken
    {
        public bool ValidateContent(object content)
        {
            return content.GetType().IsSubclassOf(typeof(JToken));
        }
        public int Count() { return 0; }
        public int UseOther(JContainer other)
        {
            return other.Count();
        }
    }
}
",
);

/// `source` with its members one line further down.
fn shifted(source: &str) -> String {
    source.replacen("    {\n", "    {\n\n", 1)
}

fn callers(_: &str, name: &str) -> bool {
    matches!(
        name,
        "ValidateContent" | "UseOther" | "Explicit" | "Instance"
    )
}

fn calls_from<'e>(edges: &'e [CallEdge], caller: &str) -> Vec<&'e str> {
    edges
        .iter()
        .filter(|edge| edge.caller == caller)
        .map(|edge| edge.callee.as_str())
        .collect()
}

/// A dirty caller over a clean `JToken`: the overlay reads the base flags
/// and matches the reindex, with no fallback to report.
#[test]
fn test_overlay_dirty_caller_private_static_of_base_matches_reindex() {
    let provider = CSharpProvider::new().unwrap();
    let container = shifted(JCONTAINER.1);
    let edits = [(JCONTAINER.0, container.as_str())];
    let parity = calls_after_edit_in(&provider, &[JTOKEN, JCONTAINER], &edits, &callers);
    assert_eq!(
        calls_from(&parity.reindex, "ValidateContent"),
        Vec::<&str>::new()
    );
    assert_eq!(calls_from(&parity.reindex, "UseOther"), ["Count"]);
    assert_eq!(parity.overlay, parity.reindex);
    let gaps = call_gaps_after_edit(&provider_parse(&provider), &[JTOKEN, JCONTAINER], &edits);
    assert_eq!(gaps, Vec::<CallGap>::new());
}

/// Both files dirty: the dirty `GetType` reads the flags of the base
/// declaration it replaced. The overlay matches the reindex, and reports
/// each refusal that rests on those flags, since the edit may change them.
/// `content.GetType()` on an `object` is an untyped member (P-recv): no
/// target reaches the access rule, and the extension retry reports that a
/// dirty file declares a `GetType` whose flags it cannot read.
#[test]
fn test_overlay_dirty_target_access_matches_reindex_and_reports_gap() {
    let provider = CSharpProvider::new().unwrap();
    let token = shifted(JTOKEN.1);
    let container = shifted(JCONTAINER.1);
    let edits = [
        (JTOKEN.0, token.as_str()),
        (JCONTAINER.0, container.as_str()),
    ];
    let parity = calls_after_edit_in(&provider, &[JTOKEN, JCONTAINER], &edits, &callers);
    assert_eq!(
        calls_from(&parity.reindex, "ValidateContent"),
        Vec::<&str>::new()
    );
    assert_eq!(calls_from(&parity.reindex, "Instance"), Vec::<&str>::new());
    assert_eq!(calls_from(&parity.reindex, "Explicit"), ["GetType"]);
    assert_eq!(parity.overlay, parity.reindex);
    let gaps = call_gaps_after_edit(&provider_parse(&provider), &[JTOKEN, JCONTAINER], &edits);
    let refused: Vec<(&str, &str)> = gaps
        .iter()
        .filter(|gap| gap.reason == GAP_CS_ACCESS_DIRTY_TARGET)
        .map(|gap| (gap.caller.as_str(), gap.member.as_str()))
        .collect();
    assert!(
        !refused.contains(&("ValidateContent", "GetType")),
        "{gaps:?}"
    );
    assert!(
        gaps.iter().any(|gap| gap.caller == "ValidateContent"
            && gap.member == "GetType"
            && gap.reason == GAP_CS_EXTENSION_DIRTY_DECLARATION),
        "{gaps:?}"
    );
    assert!(refused.contains(&("Instance", "GetType")), "{gaps:?}");
}

const TOK: (&str, &str) = (
    "Lib/Tok.cs",
    "namespace Lib
{
    public class Writer {}
    public class Conv {}
    public class Tok
    {
        private void WriteTo(Writer w) {}
        public void WriteTo(Writer w, params Conv[] c) {}
    }
}
",
);

const USER: (&str, &str) = (
    "Lib/User.cs",
    "namespace Lib
{
    public class User
    {
        public void Use(Tok t, Writer w) { t.WriteTo(w); }
    }
}
",
);

/// The 1-based callee lines of `Use` (the Linq/JToken.cs:449 shape): a private
/// first `WriteTo` hands the outside call to the one public overload.
fn write_to_lines(edges: &[CallEdge]) -> Vec<u32> {
    edges
        .iter()
        .filter(|edge| edge.caller == "Use")
        .map(|edge| edge.callee_line)
        .collect()
}

/// A dirty caller: the overlay binds the public overload like the reindex.
#[test]
fn test_overlay_dirty_caller_private_first_overload_binds_public_overload() {
    let provider = CSharpProvider::new().unwrap();
    let user = shifted(USER.1);
    let edits = [(USER.0, user.as_str())];
    let parity = calls_after_edit_in(&provider, &[TOK, USER], &edits, &|_, name| name == "Use");
    assert_eq!(write_to_lines(&parity.reindex), [8]);
    assert_eq!(parity.overlay, parity.reindex);
}

/// Both files dirty: an overload's anchor repeats, so neither dirty
/// `WriteTo` pairs with a base declaration and the overlay has no flags to
/// judge them by. The reindex binds the public overload; the overlay may
/// bind either, and reports the call in `result`. The contract is the
/// report, not the overlay's target.
#[test]
fn test_overlay_dirty_overloads_private_first_reports_gap() {
    let provider = CSharpProvider::new().unwrap();
    let tok = shifted(TOK.1);
    let user = shifted(USER.1);
    let edits = [(TOK.0, tok.as_str()), (USER.0, user.as_str())];
    let parity = calls_after_edit_in(&provider, &[TOK, USER], &edits, &|_, name| name == "Use");
    assert_eq!(write_to_lines(&parity.reindex), [9]);
    assert_eq!(write_to_lines(&parity.overlay).len(), 1);
    let gaps = call_gaps_after_edit(&provider_parse(&provider), &[TOK, USER], &edits);
    let refused: Vec<(&str, &str)> = gaps
        .iter()
        .filter(|gap| gap.reason == GAP_CS_ACCESS_DIRTY_TARGET)
        .map(|gap| (gap.caller.as_str(), gap.member.as_str()))
        .collect();
    assert_eq!(refused, [("Use", "WriteTo")], "{gaps:?}");
}

const VALUE_LIB: (&str, &str) = (
    "Lib/Lib.cs",
    "namespace Lib
{
    internal interface IXmlDocument { object CreateComment(string text); }
    public class Printer { public void WriteLine(string s) {} }
    public class Outer { public static class Inner { public static void Ping() {} } }
    public class Decoy { public static void Ping() {} }
    public class Helper { public void Run() {} }
    public class Other { public void Run() {} }
    public class Widget {}
    public static class WidgetExt { public static int Twice(this Widget w, int n) { return n; } }
}
",
);

const VALUE_USER: (&str, &str) = (
    "Lib/User.cs",
    "using System.IO;
namespace Lib
{
    public class User
    {
        private TextWriter _writer;
        private Helper _helper;
        private object Load() { return null; }
        public void Bcl() { _writer?.Close(); _writer.Dispose(); }
        public void Untyped(string d) { var doc = Load(); doc.CreateComment(d); }
        public void Library() { Console.WriteLine(\"x\"); }
        public void Path() { Outer.Inner.Ping(); }
        public void Field() { _helper.Run(); }
        public int Extension() { var x = Load(); return x.Twice(2); }
    }
}
",
);

fn value_callers(_: &str, name: &str) -> bool {
    matches!(
        name,
        "Bcl" | "Untyped" | "Library" | "Path" | "Field" | "Extension"
    )
}

/// `(callee, callee line)` of every edge out of `caller`.
fn lines_from<'e>(edges: &'e [CallEdge], caller: &str) -> Vec<(&'e str, u32)> {
    edges
        .iter()
        .filter(|edge| edge.caller == caller)
        .map(|edge| (edge.callee.as_str(), edge.callee_line))
        .collect()
}

/// A dirty caller over a clean library: every value-receiver shape binds
/// as the reindex binds it, and the overlay reports no fallback. The dirty
/// file declares no member a call names, so no extension retry meets a
/// dirty namesake. A BCL field binds nothing; an untyped receiver binds no
/// unique member of another file; a type path binds the type's member
/// (line 5), not the decoy's (line 6); a typed field binds `Helper.Run`
/// (line 7); an untyped receiver binds the one extension (line 10) at 0.5.
#[test]
fn test_overlay_dirty_caller_value_receivers_match_reindex() {
    let provider = CSharpProvider::new().unwrap();
    let user = shifted(VALUE_USER.1);
    let edits = [(VALUE_USER.0, user.as_str())];
    let files = [VALUE_LIB, VALUE_USER];
    let parity = calls_after_edit_in(&provider, &files, &edits, &value_callers);
    assert_eq!(
        lines_from(&parity.reindex, "Bcl"),
        Vec::<(&str, u32)>::new()
    );
    assert_eq!(lines_from(&parity.reindex, "Untyped"), [("Load", 9)]);
    assert_eq!(
        lines_from(&parity.reindex, "Library"),
        Vec::<(&str, u32)>::new()
    );
    assert_eq!(lines_from(&parity.reindex, "Path"), [("Ping", 5)]);
    assert_eq!(lines_from(&parity.reindex, "Field"), [("Run", 7)]);
    let twice: Vec<(u32, f32)> = parity
        .reindex
        .iter()
        .filter(|edge| edge.caller == "Extension" && edge.callee == "Twice")
        .map(|edge| (edge.callee_line, edge.confidence))
        .collect();
    assert_eq!(twice, [(10, HEURISTIC_CONFIDENCE)]);
    assert_eq!(parity.overlay, parity.reindex);
    let gaps = call_gaps_after_edit(&provider_parse(&provider), &files, &edits);
    assert_eq!(gaps, Vec::<CallGap>::new());
}

/// The overlay names an untyped receiver's extension edge by the index's
/// reason, `call:extension-heuristic`.
#[test]
fn test_overlay_untyped_receiver_extension_keeps_heuristic_reason() {
    let provider = CSharpProvider::new().unwrap();
    let user = shifted(VALUE_USER.1);
    let parity = edges_after_edit(
        &provider_parse(&provider),
        &[VALUE_LIB, VALUE_USER],
        &[(VALUE_USER.0, user.as_str())],
        RelType::Calls,
        &|_, name| name == "Extension",
    );
    let twice = |edges: &[parity::ReasonedEdge]| -> Vec<(f32, String)> {
        edges
            .iter()
            .filter(|edge| edge.edge.callee == "Twice")
            .map(|edge| (edge.edge.confidence, edge.reason.clone()))
            .collect()
    };
    let want = [(HEURISTIC_CONFIDENCE, HEURISTIC_REASON.to_string())];
    assert_eq!(twice(&parity.reindex), want);
    assert_eq!(twice(&parity.overlay), want);
}

/// A dirty file that declares the extension: the overlay cannot read its
/// flags, binds no `Twice` where the reindex binds one, and reports the
/// call ([`GAP_CS_EXTENSION_DIRTY_DECLARATION`]).
#[test]
fn test_overlay_dirty_extension_declaration_untyped_receiver_reports_gap() {
    let provider = CSharpProvider::new().unwrap();
    let lib = format!("\n{}", VALUE_LIB.1);
    let user = shifted(VALUE_USER.1);
    let edits = [(VALUE_LIB.0, lib.as_str()), (VALUE_USER.0, user.as_str())];
    let files = [VALUE_LIB, VALUE_USER];
    let parity = calls_after_edit_in(&provider, &files, &edits, &|_, name| name == "Extension");
    let twice = |edges: &[CallEdge]| edges.iter().filter(|e| e.callee == "Twice").count();
    assert_eq!(twice(&parity.reindex), 1);
    assert_eq!(twice(&parity.overlay), 0);
    let gaps = call_gaps_after_edit(&provider_parse(&provider), &files, &edits);
    assert!(
        gaps.iter().any(|gap| gap.caller == "Extension"
            && gap.member == "Twice"
            && gap.reason == GAP_CS_EXTENSION_DIRTY_DECLARATION),
        "{gaps:?}"
    );
}
