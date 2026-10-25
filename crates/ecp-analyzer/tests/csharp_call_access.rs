//! C# member accessibility in call binding (codex ruling P-ext2 C): an
//! instance call never binds a static method, and a `private` member binds
//! only from its own type and the types nested in it.
//!
//! The fixture mirrors Newtonsoft.Json: `JContainer : JToken` calls
//! `content.GetType()` on an `object`, which the parser writes as a bare
//! `GetType`. `JToken` declares `private static string GetType(JToken)`.
//! Before P-ext2 the heritage tier bound the call to it, and the chain then
//! typed `.IsSubclassOf(..)` on a `string` receiver.

mod receiver_typing_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use receiver_typing_support::{build, parse_all};

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
        public class Nested
        {
            public string FromNested(JToken t)
            {
                return JToken.GetType(t);
            }
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

fn graph() -> ZeroCopyGraph {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    build(parse_all(&provider, &[JTOKEN, JCONTAINER]))
}

/// The callee names of every `Calls` edge out of `caller`.
fn callees(graph: &ZeroCopyGraph, caller: &str) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    let mut names: Vec<String> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .map(|e| {
            graph.nodes[e.target as usize]
                .name
                .resolve(pool)
                .to_string()
        })
        .collect();
    names.sort();
    names
}

/// Regression (JContainer.cs:765): the bare `GetType` of a derived type
/// never reaches the base's private static, and no `string` receiver types
/// the rest of the chain.
#[test]
fn test_resolve_call_derived_type_bare_call_private_static_of_base_emits_no_edge() {
    let graph = graph();
    assert_eq!(callees(&graph, "ValidateContent"), Vec::<String>::new());
    let pool = graph.string_pool.as_slice();
    let string_receiver: Vec<&str> = graph
        .blind_spots
        .iter()
        .map(|b| b.hint.resolve(pool))
        .filter(|hint| hint.contains("`string`") || hint.contains("`String`"))
        .collect();
    assert_eq!(string_receiver, Vec::<&str>::new());
}

/// An instance call (`t.GetType()`) never binds a static method, even one
/// its own type declares.
#[test]
fn test_resolve_call_instance_call_static_method_emits_no_edge() {
    assert_eq!(callees(&graph(), "Instance"), Vec::<String>::new());
}

/// The explicit static call inside the declaring type, and from a type
/// nested in it, still binds the private static.
#[test]
fn test_resolve_call_explicit_static_call_in_own_or_nested_type_binds() {
    let graph = graph();
    assert_eq!(callees(&graph, "Explicit"), ["GetType"]);
    assert_eq!(callees(&graph, "FromNested"), ["GetType"]);
}

/// A legal instance call to a public instance method still binds.
#[test]
fn test_resolve_call_instance_call_public_instance_method_binds() {
    assert_eq!(callees(&graph(), "UseOther"), ["Count"]);
}

/// The target line of every `Calls` edge out of `caller` to `callee`.
fn target_lines(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<u32> {
    let pool = graph.string_pool.as_slice();
    let mut lines: Vec<u32> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .map(|e| &graph.nodes[e.target as usize])
        .filter(|n| n.name.resolve(pool) == callee)
        .map(|n| n.span.0)
        .collect();
    lines.sort_unstable();
    lines
}

/// `Tok` declares `WriteTo` twice, on the two rows `rows` gives, in that
/// order: `private` on `private_row`, `public` with a `params` array on the
/// other. `User.Use` calls `t.WriteTo(w)`; `Tok.Own` calls `WriteTo(w)`.
fn write_to_graph(private_first: bool) -> ZeroCopyGraph {
    let private = "        private void WriteTo(Writer w) {}";
    let public = "        public void WriteTo(Writer w, params Conv[] c) {}";
    let (first, second) = if private_first {
        (private, public)
    } else {
        (public, private)
    };
    let tok = format!(
        "namespace Lib
{{
    public class Writer {{}}
    public class Conv {{}}
    public class Tok
    {{
{first}
{second}
        public void Own(Writer w) {{ WriteTo(w); }}
    }}
}}
"
    );
    let user = "namespace Lib
{
    public class User
    {
        public void Use(Tok t, Writer w) { t.WriteTo(w); }
    }
}
";
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    build(parse_all(
        &provider,
        &[("Lib/Tok.cs", &tok), ("Lib/User.cs", user)],
    ))
}

/// Regression (Linq/JToken.cs:449): the first `WriteTo` is private, so a
/// call from another type binds the one public overload, as C# does.
#[test]
fn test_resolve_call_private_first_overload_outside_type_binds_public_overload() {
    assert_eq!(target_lines(&write_to_graph(true), "Use", "WriteTo"), [7]);
}

/// With the public overload first, the outside call binds it.
#[test]
fn test_resolve_call_public_first_overload_outside_type_binds_public_overload() {
    assert_eq!(target_lines(&write_to_graph(false), "Use", "WriteTo"), [6]);
}

/// Inside its own type the private first overload stays reachable.
#[test]
fn test_resolve_call_private_first_overload_own_type_binds_private_overload() {
    assert_eq!(target_lines(&write_to_graph(true), "Own", "WriteTo"), [6]);
}

/// Two reachable overloads after a private first one are not told apart:
/// no edge.
#[test]
fn test_resolve_call_private_first_two_public_overloads_outside_type_emits_no_edge() {
    let tok = "namespace Lib
{
    public class Writer {}
    public class Tok
    {
        private void WriteTo(Writer w) {}
        public void WriteTo(Writer w, int depth) {}
        public void WriteTo(Writer w, string name) {}
    }
}
";
    let user = "namespace Lib
{
    public class User
    {
        public void Use(Tok t, Writer w) { t.WriteTo(w, 1); }
    }
}
";
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graph = build(parse_all(
        &provider,
        &[("Lib/Tok.cs", tok), ("Lib/User.cs", user)],
    ));
    assert_eq!(target_lines(&graph, "Use", "WriteTo"), Vec::<u32>::new());
}
