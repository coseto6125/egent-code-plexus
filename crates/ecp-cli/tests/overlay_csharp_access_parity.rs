//! The C# access rule on the session overlay (codex ruling P-ext2 C): a
//! dirty caller binds the same calls as a full reindex. An instance call
//! never binds a static method, and a private member binds only from its
//! own type and the types nested in it. The fixture is the Newtonsoft.Json
//! shape of `crates/ecp-analyzer/tests/csharp_call_access.rs`.

#[path = "common/parity.rs"]
mod parity;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::session::view::{CallGap, GAP_CS_ACCESS_DIRTY_TARGET};
use parity::{call_gaps_after_edit, calls_after_edit_in, provider_parse, CallEdge};

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
        refused.contains(&("ValidateContent", "GetType")),
        "{gaps:?}"
    );
    assert!(refused.contains(&("Instance", "GetType")), "{gaps:?}");
}
