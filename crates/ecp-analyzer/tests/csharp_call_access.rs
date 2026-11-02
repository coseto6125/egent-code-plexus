//! C# member accessibility in call binding (codex ruling P-ext2 C): an
//! instance call never binds a static method, and a `private` member binds
//! only from its own type and the types nested in it.
//!
//! The fixture mirrors Newtonsoft.Json: `JContainer : JToken` calls
//! `content.GetType()` on an `object`. `JToken` declares `private static
//! string GetType(JToken)`. Before P-ext2 the parser wrote a bare `GetType`,
//! the heritage tier bound the call to it, and the chain then typed
//! `.IsSubclassOf(..)` on a `string` receiver. Since P-recv the parser
//! writes an untyped member, which binds no member by its name.

mod receiver_typing_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use receiver_typing_support::{build, parse_all};
use std::path::Path;

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

/// Regression (JContainer.cs:765): `GetType` on an `object` in a derived
/// type never reaches the base's private static, and no `string` receiver
/// types the rest of the chain.
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

/// The graph of one file `Lib/App.cs` holding `source`.
fn one_file(source: &str) -> ZeroCopyGraph {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    build(parse_all(&provider, &[("Lib/App.cs", source)]))
}

/// Stands for (B3): a chained member call obeys the access rule a plain
/// call obeys. `R` declares a private `Read()` (row 4) before a public
/// `Read(int)` (row 5); `User.Use` calls `Maker.Make().Read(1)` from outside
/// `R`, so it binds the public overload. Before the fix the chain bound the
/// first, private declaration.
#[test]
fn test_resolve_chain_private_first_overload_outside_type_binds_public_overload() {
    let graph = one_file(
        "namespace Lib
{
    public class R
    {
        private void Read() {}
        public void Read(int x) {}
    }
    public class Maker
    {
        public static R Make() { return new R(); }
    }
    public class User
    {
        public void Use() { Maker.Make().Read(1); }
    }
}
",
    );
    assert_eq!(target_lines(&graph, "Use", "Read"), [5]);
    assert_eq!(target_lines(&graph, "Use", "Make"), [9]);
}

/// Stands for (B3): a chained call to a private member of another type, with
/// no reachable overload, binds nothing. Before the fix it bound the private
/// `Read` (row 4).
#[test]
fn test_resolve_chain_private_only_member_outside_type_emits_no_edge() {
    let graph = one_file(
        "namespace Lib
{
    public class R
    {
        private void Read() {}
    }
    public class Maker
    {
        public static R Make() { return new R(); }
    }
    public class User
    {
        public void Use() { Maker.Make().Read(); }
    }
}
",
    );
    assert_eq!(target_lines(&graph, "Use", "Read"), Vec::<u32>::new());
}

/// Stands for (F6, "Color Color", C# spec 12.8.7.2): a property named as its
/// type is the value and the type at once. `Svc.Logger` of type `Logger`:
/// `Logger.Create()` binds the static `Create` (row 4) and `Logger.Log()`
/// the instance `Log` (row 5). Before the fix the site was an instance call,
/// and the access rule refused the static `Create`.
#[test]
fn test_resolve_call_color_color_property_binds_static_and_instance_member() {
    let graph = one_file(
        "namespace Lib
{
    public class Logger
    {
        public static Logger Create() { return null; }
        public void Log() {}
    }
    public class Svc
    {
        public Logger Logger { get; }
        public void Run()
        {
            Logger.Create();
            Logger.Log();
        }
    }
}
",
    );
    assert_eq!(target_lines(&graph, "Run", "Create"), [4]);
    assert_eq!(target_lines(&graph, "Run", "Log"), [5]);
}

/// Stands for (F6): a value whose type has another name stays an instance
/// call. `Logger log` is no `Logger` spelling, so `log.Create()` binds no
/// static `Create`; `log.Log()` binds the instance `Log`. Passes before the
/// fix as well: it guards the shape beside the rule.
#[test]
fn test_resolve_call_differently_named_value_static_member_emits_no_edge() {
    let graph = one_file(
        "namespace Lib
{
    public class Logger
    {
        public static Logger Create() { return null; }
        public void Log() {}
    }
    public class Svc
    {
        public void Run(Logger log)
        {
            log.Create();
            log.Log();
        }
    }
}
",
    );
    assert_eq!(target_lines(&graph, "Run", "Create"), Vec::<u32>::new());
    assert_eq!(target_lines(&graph, "Run", "Log"), [5]);
}

// ---- FU 755942c857a3: explicit interface implementations ----

/// `(file, 0-based row, confidence)` of a `Calls` target.
type Row = (String, u32, f32);

/// The `Calls` targets named `callee` from `caller`, sorted.
fn rows_of(
    edges: impl Iterator<Item = (u32, u32, f32)>,
    name: impl Fn(u32) -> String,
    file: impl Fn(u32) -> String,
    row: impl Fn(u32) -> u32,
    caller: &str,
    callee: &str,
) -> Vec<Row> {
    let mut rows: Vec<Row> = edges
        .filter(|&(source, target, _)| name(source) == caller && name(target) == callee)
        .map(|(_, target, confidence)| (file(target), row(target), confidence))
        .collect();
    rows.sort_by(|a, b| a.partial_cmp(b).unwrap());
    rows
}

/// [`rows_of`] in a full build.
fn remove_rows(graph: &ZeroCopyGraph, caller: &str) -> Vec<Row> {
    let pool = graph.string_pool.as_slice();
    let node = |id: u32| &graph.nodes[id as usize];
    rows_of(
        graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Calls)
            .map(|e| (e.source, e.target, e.confidence)),
        |id| node(id).name.resolve(pool).to_string(),
        |id| {
            graph.files[node(id).file_idx as usize]
                .path
                .resolve(pool)
                .to_string()
        },
        |id| node(id).span.0,
        caller,
        "Remove",
    )
}

/// [`rows_of`] in a merged graph.
fn merged_remove_rows(merged: &MergedGraph<'_>, caller: &str) -> Vec<Row> {
    let node = |id: u32| merged.node(id).unwrap();
    rows_of(
        merged
            .all_edges()
            .filter(|e| e.rel_type() == RelType::Calls)
            .map(|e| (e.source, e.target, e.confidence())),
        |id| node(id).name(merged).to_string(),
        |id| node(id).file_path(merged).unwrap().to_string(),
        |id| node(id).start_line() - 1,
        caller,
        "Remove",
    )
}

/// A file `Lib/<name>.cs` declaring `decl` with `body` in namespace `Lib`;
/// the first body row is row 4.
fn lib_file(name: &str, decl: &str, body: &str) -> (String, String) {
    (
        format!("Lib/{name}.cs"),
        format!("namespace Lib\n{{\n    {decl}\n    {{\n{body}    }}\n}}\n"),
    )
}

const USE_BODY: &str = "        public void F(Property p) { p.Remove(); }
        public void G(Container c) { c.Remove(); }
        public void H(Property p) { p.Remove(1); }
        public void I(IBag b) { b.Remove(null); }
        public void J(Container c) { ((IBag)c).Remove(null); }
";

/// The FU's shape A: `Container : Token, IBag` implements `IBag.Remove`
/// explicitly, `Token` declares `public void Remove()` (row 4), and
/// `Property : Container`. `bag` is `IBag`'s body, `container` `Container`'s.
fn bag_files(bag: &str, container: &str) -> Vec<(String, String)> {
    vec![
        lib_file("IBag", "public interface IBag", bag),
        lib_file(
            "Token",
            "public abstract class Token",
            "        public void Remove() {}\n",
        ),
        lib_file(
            "Container",
            "public class Container : Token, IBag",
            container,
        ),
        lib_file("Property", "public class Property : Container", ""),
        lib_file("Use", "public class Use", USE_BODY),
    ]
}

const BAG_REMOVE: &str = "        bool Remove(object o);\n";
const EXPLICIT_REMOVE: &str = "        bool IBag.Remove(object o) { return false; }\n";

fn graph_of_files(files: &[(String, String)]) -> ZeroCopyGraph {
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    build(parse_all(&provider, &refs))
}

fn token_remove(confidence: f32) -> Vec<Row> {
    vec![("Lib/Token.cs".to_string(), 4, confidence)]
}

fn ibag_remove() -> Vec<Row> {
    vec![("Lib/IBag.cs".to_string(), 4, 0.85)]
}

/// Contract (FU 755942c857a3): C# never reaches an explicit interface
/// implementation by its name, and a class's member lookup never stops at
/// an interface's declaration. `p.Remove()` (`F`, the implementation on a
/// nearer base) and `c.Remove()` (`G`, on the receiver's own type) bind
/// the inherited `Token.Remove()` at the heritage tier, 0.8. Before the fix
/// `G` met the explicit implementation as `Container`'s own `Remove`, the
/// access rule refused it, and `F` met `IBag.Remove` on a second branch:
/// neither had a row.
#[test]
fn test_resolve_call_explicit_impl_on_base_binds_inherited_member() {
    let graph = graph_of_files(&bag_files(BAG_REMOVE, EXPLICIT_REMOVE));
    assert_eq!(remove_rows(&graph, "F"), token_remove(0.8), "F");
    assert_eq!(remove_rows(&graph, "G"), token_remove(0.8), "G");
}

/// Known gap (arity, FU 53efbbdb2c8f): no parameter count is checked, so
/// `p.Remove(1)` binds the parameterless `Token.Remove()`, which C#
/// rejects. This pins the gap; it is not the correct target.
#[test]
fn test_resolve_call_explicit_impl_on_base_wrong_arity_binds_known_gap() {
    let graph = graph_of_files(&bag_files(BAG_REMOVE, EXPLICIT_REMOVE));
    assert_eq!(remove_rows(&graph, "H"), token_remove(0.8));
}

/// A call through the interface keeps binding the interface's declaration:
/// an `IBag` receiver (`I`) and a cast to `IBag` (`J`).
#[test]
fn test_resolve_call_interface_receiver_still_binds_interface_member() {
    let graph = graph_of_files(&bag_files(BAG_REMOVE, EXPLICIT_REMOVE));
    assert_eq!(remove_rows(&graph, "I"), ibag_remove(), "I");
    assert_eq!(remove_rows(&graph, "J"), ibag_remove(), "J");
}

/// Shape E: `IBag` declares `void Remove()` and `Container` implements it
/// only through the inherited `Token.Remove()`. The lookup on `Container`
/// meets `Token`'s member and `IBag`'s declaration on two branches; the
/// interface's drops out, so `G` binds `Token.Remove` at 0.8. Before the
/// fix: no row.
#[test]
fn test_resolve_call_interface_member_implemented_by_base_binds_base() {
    let graph = graph_of_files(&bag_files("        void Remove();\n", ""));
    assert_eq!(remove_rows(&graph, "G"), token_remove(0.8));
    assert_eq!(remove_rows(&graph, "F"), token_remove(0.8));
}

/// A bare `Remove()` inside `Container` never binds the explicit
/// implementation, which C# does not reach by its name. Before the fix the
/// same-file tier bound it, and the access rule admitted it as a private
/// member of the caller's own type. (C# binds `Token.Remove()`; whether the
/// bare-call tiers reach it is outside this rule, so only the wrong target
/// is asserted absent.)
#[test]
fn test_resolve_call_bare_call_inside_type_never_binds_explicit_impl() {
    let container = format!("{EXPLICIT_REMOVE}        public void K() {{ Remove(); }}\n");
    let graph = graph_of_files(&bag_files(BAG_REMOVE, &container));
    let rows = remove_rows(&graph, "K");
    assert!(
        rows.iter().all(|(file, _, _)| file != "Lib/Container.cs"),
        "{rows:?}"
    );
}

/// Two explicit implementations of one name (`IBag.Remove`, then
/// `IOther.Remove`, a later overload of the first): neither is reachable by
/// name, so `x.Remove()` on `Pair` and `s.Remove()` on `Sub : Pair` bind
/// `Token.Remove` at 0.8. Before the fix: no row.
#[test]
fn test_resolve_call_two_explicit_impls_bind_inherited_member() {
    let mut files = bag_files(BAG_REMOVE, EXPLICIT_REMOVE);
    files.push(lib_file(
        "IOther",
        "public interface IOther",
        "        void Remove(object o);\n",
    ));
    files.push(lib_file(
        "Pair",
        "public class Pair : Token, IBag, IOther",
        "        bool IBag.Remove(object o) { return false; }\n        void IOther.Remove(object o) {}\n",
    ));
    files.push(lib_file("Sub", "public class Sub : Pair", ""));
    files.push(lib_file(
        "UsePair",
        "public class UsePair",
        "        public void Q(Pair x) { x.Remove(); }\n        public void R(Sub s) { s.Remove(); }\n",
    ));
    let graph = graph_of_files(&files);
    assert_eq!(remove_rows(&graph, "Q"), token_remove(0.8), "Q");
    assert_eq!(remove_rows(&graph, "R"), token_remove(0.8), "R");
}

/// The overlay rows of `callers` over the full build of `files`, with the
/// files `dirty` re-parsed (content unchanged), beside the full build's.
fn overlay_and_full(files: &[(String, String)], dirty: &[&str], callers: &[&str]) {
    let full = graph_of_files(files);
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&full).unwrap();
    let base = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let inputs: Vec<OverlayFileInput> = files
        .iter()
        .filter(|(path, _)| dirty.contains(&path.as_str()))
        .map(|(path, src)| {
            OverlayFileInput::from(
                provider
                    .parse_file(Path::new(path), src.as_bytes())
                    .unwrap(),
            )
        })
        .collect();
    let view = OverlayView::build(base, &inputs).expect("a dirty view");
    let merged = MergedGraph::new(base, Some(&view));
    for caller in callers {
        assert_eq!(
            merged_remove_rows(&merged, caller),
            remove_rows(&full, caller),
            "{caller}, dirty {dirty:?}"
        );
        assert_eq!(remove_rows(&full, caller), token_remove(0.8), "{caller}");
    }
}

/// Stands for: the overlay reads the explicit-implementation code as the
/// index does, from the archived flags of a clean `Container` (the index
/// keeps code 6 in `note_csharp_method_flags`) and from the fresh parse of
/// a dirty one. Before the fix both gave no row.
#[test]
fn test_overlay_view_explicit_impl_matches_full_build() {
    let files = bag_files(BAG_REMOVE, EXPLICIT_REMOVE);
    overlay_and_full(&files, &["Lib/Use.cs"], &["F", "G"]);
    overlay_and_full(&files, &["Lib/Use.cs", "Lib/Container.cs"], &["F", "G"]);
}

/// Stands for: a dirty file's second explicit implementation, a later
/// overload of the first, stays hidden in the overlay too (its flags come
/// from its own parse, never the 0 an anchorless overload reads).
#[test]
fn test_overlay_view_two_explicit_impls_dirty_later_overload_stays_hidden() {
    let mut files = bag_files(BAG_REMOVE, EXPLICIT_REMOVE);
    files.push(lib_file(
        "IOther",
        "public interface IOther",
        "        void Remove(object o);\n",
    ));
    files.push(lib_file(
        "Pair",
        "public class Pair : Token, IBag, IOther",
        "        bool IBag.Remove(object o) { return false; }\n        void IOther.Remove(object o) {}\n",
    ));
    files.push(lib_file(
        "UsePair",
        "public class UsePair",
        "        public void Q(Pair x) { x.Remove(); }\n",
    ));
    overlay_and_full(&files, &["Lib/UsePair.cs", "Lib/Pair.cs"], &["Q"]);
}
