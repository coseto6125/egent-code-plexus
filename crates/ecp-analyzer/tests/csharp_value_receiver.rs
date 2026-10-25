//! C# member calls on a value receiver (codex ruling P-recv). A receiver
//! written in the call is never `this`: only `M()`, `this.M()` and
//! `base.M()` reach the caller's own type and its bases. A value whose type
//! the parser reads by name (a typed local, parameter, field or property,
//! `new T()`, a keyword type) resolves as that type's member. A value of
//! unknown type binds no member by its name alone, in the same file or
//! anywhere else; only one applicable C# extension method may bind it, at
//! [`HEURISTIC_CONFIDENCE`] with [`HEURISTIC_REASON`].
//!
//! Regression: `JsonTextWriter.CloseBufferAndWriter` (Newtonsoft.Json,
//! JsonTextWriter.cs:191) calls `_writer?.Dispose()` on a BCL `TextWriter`
//! field, which bound the base `JsonWriter.Dispose(bool)`; and
//! `XmlNodeConverter.cs` `_document.CreateComment(d)` on a BCL
//! `XmlDocument` bound the same-file `IXmlDocument.CreateComment`.

mod receiver_typing_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::csharp_extension::{HEURISTIC_CONFIDENCE, HEURISTIC_REASON};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::CallSite;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use receiver_typing_support::{build, parse_all};
use std::path::Path;

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    build(parse_all(&provider, files))
}

/// One `Calls` edge out of a caller: `Owner.callee`, the callee's file, the
/// confidence and the reason.
#[derive(Debug, PartialEq)]
struct Call {
    target: String,
    file: String,
    confidence: f32,
    reason: String,
}

/// Every `Calls` edge out of the nodes named `caller`, sorted and deduplicated.
fn calls_from(graph: &ZeroCopyGraph, caller: &str) -> Vec<Call> {
    let pool = &graph.string_pool;
    let mut calls: Vec<Call> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            Call {
                target: format!(
                    "{}.{}",
                    target.owner_class.resolve(pool),
                    target.name.resolve(pool)
                ),
                file: graph.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string(),
                confidence: e.confidence,
                reason: e.reason.resolve(pool).to_string(),
            }
        })
        .collect();
    calls.sort_by(|a, b| a.target.cmp(&b.target).then(a.file.cmp(&b.file)));
    calls.dedup();
    calls
}

/// `Owner.callee` of every `Calls` edge out of `caller`.
fn targets(graph: &ZeroCopyGraph, caller: &str) -> Vec<String> {
    calls_from(graph, caller)
        .into_iter()
        .map(|call| call.target)
        .collect()
}

/// The decoded call sites the parser records for the node named `caller`.
fn sites(path: &str, source: &str, caller: &str) -> Vec<String> {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let local = provider
        .parse_file(Path::new(path), source.as_bytes())
        .expect("parse_file");
    let mut sites: Vec<String> = local
        .nodes
        .iter()
        .filter(|n| n.name == caller)
        .flat_map(|n| {
            n.calls
                .iter()
                .map(|raw| format!("{:?}", CallSite::parse(raw)))
        })
        .collect();
    sites.sort();
    sites
}

const BASE: (&str, &str) = (
    "Lib/Base.cs",
    "namespace Lib
{
    public abstract class Base
    {
        public virtual void Close() {}
        protected virtual void Dispose(bool disposing) {}
    }
}
",
);

const OTHER: (&str, &str) = (
    "Lib/Other.cs",
    "namespace Lib
{
    public class Other
    {
        public void Close() {}
        protected void Dispose(bool disposing) {}
        public void Flush() {}
    }
}
",
);

/// Regression (JsonTextWriter.cs:191): `_writer` is a BCL `TextWriter`, so
/// `_writer?.Close()`, `_writer.Dispose()` and `this._writer.Flush()` call
/// no method of the graph: not the caller's own `Close`, not its base's
/// `Dispose(bool)`, not another type's namesake. HEAD bound the own `Close`
/// (same file), `Base.Dispose` (heritage) and the unique `Other.Flush`.
#[test]
fn test_resolve_call_bcl_field_receiver_binds_no_own_base_or_other_member() {
    let text = "using System.IO;
namespace Lib
{
    public class Text : Base
    {
        private readonly TextWriter _writer;
        public override void Close() { Shut(); }
        private void Shut()
        {
            _writer?.Close();
            _writer.Dispose();
            this._writer.Flush();
        }
    }
}
";
    let graph = graph(&[BASE, OTHER, ("Lib/Text.cs", text)]);
    assert_eq!(targets(&graph, "Shut"), Vec::<String>::new());
    assert_eq!(
        targets(&graph, "Close"),
        ["Text.Shut"],
        "the bare call still binds"
    );
}

/// A field or property whose type is no plain type name (`List<Item>`,
/// `Dictionary<..>`, `Item?`) is a value of unknown type: no edge to the
/// caller's own `Add`, `Clear` or `Remove`, which HEAD bound (same file).
#[test]
fn test_resolve_call_unresolved_type_property_and_field_receivers_bind_nothing() {
    let bag = "using System.Collections.Generic;
namespace Lib
{
    public class Item {}
    public class Bag
    {
        public List<Item> Items { get; }
        private Dictionary<string, Item> _map;
        private Item? _maybe;
        public void Add(Item i) {}
        public void Clear() {}
        public void Remove(Item i) {}
        public void Fill(Item i)
        {
            Items.Add(i);
            _map.Clear();
            _maybe?.Remove(i);
        }
    }
}
";
    assert_eq!(
        targets(&graph(&[("Lib/Bag.cs", bag)]), "Fill"),
        Vec::<String>::new()
    );
}

/// `var w = ..`, a `foreach` variable and an `object` parameter are values
/// of unknown type: no edge to the caller's own `Flush`, which HEAD bound.
/// The bare `Open()` still binds.
#[test]
fn test_resolve_call_unknown_local_receiver_binds_nothing() {
    let sink = "namespace Lib
{
    public class Sink
    {
        public void Flush() {}
        public object Open() { return null; }
        public void Use(object raw)
        {
            var w = Open();
            w.Flush();
            foreach (var each in new[] { raw }) { each.Flush(); }
            raw.Flush();
        }
    }
}
";
    assert_eq!(
        targets(&graph(&[("Lib/Sink.cs", sink)]), "Use"),
        ["Sink.Open"]
    );
}

/// A parameter of a library type keeps its type (`TextWriter.Flush`), never
/// the bare member: the resolver finds no `TextWriter` and binds nothing,
/// though the caller declares its own `Flush`. Guard: HEAD did the same.
#[test]
fn test_resolve_call_external_typed_parameter_keeps_known_type() {
    let writer = "using System.IO;
namespace Lib
{
    public class Log
    {
        public void Flush() {}
        public void Use(TextWriter w) { w.Flush(); }
    }
}
";
    assert_eq!(
        sites("Lib/Log.cs", writer, "Use"),
        [format!("{:?}", CallSite::Plain("TextWriter.Flush"))]
    );
    assert_eq!(
        targets(&graph(&[("Lib/Log.cs", writer)]), "Use"),
        Vec::<String>::new()
    );
}

/// `a.b.M()` with `a` a field, a parameter or `this._a`: a value path. The
/// parser does not follow member types through another type, so no edge;
/// HEAD bound the caller's own `Flush` (same file).
#[test]
fn test_resolve_call_dotted_value_receiver_binds_nothing() {
    let node = "namespace Lib
{
    public class Pipe { public void Flush() {} }
    public class Host { public Pipe Pipe; }
    public class Node
    {
        private Host _host;
        public void Flush() {}
        public void Use(Host h)
        {
            _host.Pipe.Flush();
            h.Pipe.Flush();
            this._host.Pipe.Flush();
        }
    }
}
";
    assert_eq!(
        targets(&graph(&[("Lib/Node.cs", node)]), "Use"),
        Vec::<String>::new()
    );
}

const IXML: &str = "namespace Lib
{
    internal interface IXmlDocument
    {
        object CreateComment(string text);
    }
}
";

const CONVERTER: &str = "namespace Lib
{
    internal class Converter
    {
        private object Load() { return null; }
        public void Build(string d)
        {
            var document = Load();
            document.CreateComment(d);
        }
    }
}
";

/// Codex ruling P-recv Q3: "same file and same name" never builds a member
/// edge for an untyped receiver. HEAD bound `IXmlDocument.CreateComment`.
#[test]
fn test_resolve_call_untyped_receiver_same_file_interface_member_binds_nothing() {
    let same_file = format!("{}{}", IXML, CONVERTER);
    assert_eq!(
        targets(&graph(&[("Lib/Converter.cs", &same_file)]), "Build"),
        ["Converter.Load"]
    );
}

/// The same candidate in another file, and the only `CreateComment` of the
/// project: the global tier does not bring it back. HEAD bound it there.
#[test]
fn test_resolve_call_untyped_receiver_unique_member_in_other_file_binds_nothing() {
    let graph = graph(&[
        ("Lib/IXmlDocument.cs", IXML),
        ("Lib/Converter.cs", CONVERTER),
    ]);
    assert_eq!(targets(&graph, "Build"), ["Converter.Load"]);
}

/// The XmlNodeConverter shape: a wrapper implements `IXmlDocument` and its
/// own `CreateComment` calls the BCL `_document.CreateComment(d)`. Neither
/// the interface member nor the wrapper's own binds; the bare `Wrap` does.
/// Guard: the untyped-member experiment bound the interface member (+16
/// edges on Newtonsoft.Json).
#[test]
fn test_resolve_call_bcl_field_beside_same_file_interface_binds_only_bare_call() {
    let wrapper = "using System.Xml;
namespace Lib
{
    internal interface IXmlDocument
    {
        object CreateComment(string text);
    }
    internal class XmlDocumentWrapper : IXmlDocument
    {
        private readonly XmlDocument _document;
        public object CreateComment(string data) { return Wrap(_document.CreateComment(data)); }
        private object Wrap(object node) { return node; }
    }
}
";
    let graph = graph(&[("Lib/XmlNodeConverter.cs", wrapper)]);
    assert_eq!(
        targets(&graph, "CreateComment"),
        ["XmlDocumentWrapper.Wrap"]
    );
}

/// `M()`, `this.M()`, `base.M()` and an inherited bare call keep their
/// bindings. Guard: HEAD bound the same.
#[test]
fn test_resolve_call_implicit_this_and_base_calls_still_bind() {
    let zoo = "namespace Lib
{
    public class Animal
    {
        public virtual void Eat() {}
        public void Breathe() {}
    }
    public class Dog : Animal
    {
        public override void Eat() {}
        public void Bark() {}
        public void Live()
        {
            Bark();
            this.Bark();
            base.Eat();
            Breathe();
        }
    }
}
";
    // `Bark()` binds same-file and `this.Bark()` owned: two edges of
    // different confidence to one target.
    let mut live = targets(&graph(&[("Lib/Zoo.cs", zoo)]), "Live");
    live.dedup();
    assert_eq!(live, ["Animal.Breathe", "Animal.Eat", "Dog.Bark"]);
}

const PATHS: &str = "namespace Lib
{
    public class Outer
    {
        public static class Inner
        {
            public static void Ping() {}
        }
    }
    public class Caller
    {
        public void ViaPath() { Outer.Inner.Ping(); }
        public void ViaValuePath() { Settings.Current.Ping(); }
    }
}
";

/// `Outer.Inner.Ping()` binds through the type its path names, though a
/// second `Ping` makes the bare name ambiguous (HEAD bound nothing).
#[test]
fn test_resolve_call_type_path_receiver_binds_through_type() {
    let decoy = (
        "Lib/Decoy.cs",
        "namespace Lib { public class Decoy { public static void Ping() {} } }\n",
    );
    let graph = graph(&[("Lib/Paths.cs", PATHS), decoy]);
    assert_eq!(targets(&graph, "ViaPath"), ["Inner.Ping"]);
}

/// A dotted receiver whose last segment names no type (`Settings.Current`,
/// an inherited value) binds nothing, though `Ping` is the project's only
/// one: a unique name is no type-path evidence. HEAD bound `Inner.Ping`.
#[test]
fn test_resolve_call_unproven_type_path_unique_member_binds_nothing() {
    assert_eq!(
        targets(&graph(&[("Lib/Paths.cs", PATHS)]), "ViaValuePath"),
        Vec::<String>::new()
    );
}

/// A receiver that is no local or member and names no project type
/// (`Console`, or a property a base in another file declares) binds no
/// project `WriteLine` by its name. HEAD bound the unique `Printer.WriteLine`.
#[test]
fn test_resolve_call_receiver_naming_no_project_type_binds_nothing() {
    let log = "namespace App
{
    public class Printer { public void WriteLine(string s) {} }
    public class Log
    {
        public void ViaConsole() { Console.WriteLine(\"x\"); }
    }
}
";
    assert_eq!(
        targets(&graph(&[("App/Log.cs", log)]), "ViaConsole"),
        Vec::<String>::new()
    );
}

/// A field or property declared with a plain type name, and a `var`
/// initialized by `new Helper()`, type the call: `Helper.Run`, not the
/// decoy's. HEAD left the bare `Run` ambiguous and bound nothing.
#[test]
fn test_resolve_call_typed_field_and_property_receiver_binds_declared_type() {
    let types = (
        "Lib/Types.cs",
        "namespace Lib
{
    public class Helper { public void Run() {} }
    public class Decoy { public void Run() {} }
}
",
    );
    let user = (
        "Lib/User.cs",
        "namespace Lib
{
    public class User
    {
        private Helper _helper;
        public Helper Tool { get; }
        public void ViaField() { _helper.Run(); }
        public void ViaProperty() { Tool.Run(); }
        public void ViaConditional() { _helper?.Run(); }
        public void ViaVarNew() { var h = new Helper(); h.Run(); }
    }
}
",
    );
    let graph = graph(&[types, user]);
    assert_eq!(targets(&graph, "ViaField"), ["Helper.Run"]);
    assert_eq!(targets(&graph, "ViaProperty"), ["Helper.Run"]);
    assert_eq!(
        targets(&graph, "ViaConditional"),
        ["Helper.Run"],
        "`x?.M()` reads the receiver from the tree"
    );
    let runs: Vec<String> = targets(&graph, "ViaVarNew")
        .into_iter()
        .filter(|target| target.ends_with(".Run"))
        .collect();
    assert_eq!(runs, ["Helper.Run"]);
}

/// `new Foo().Run()` is a `Foo`; `string.Join(..)` calls the library type,
/// never the caller's own `Join`. HEAD left `Run` ambiguous (no edge) and
/// bound the own `Join`.
#[test]
fn test_resolve_call_new_object_and_keyword_type_receivers_bind_their_type() {
    let shop = "namespace Lib
{
    public class Foo { public void Run() {} }
    public class Bar { public void Run() {} }
    public class Caller
    {
        public string Join(string a, string b) { return a; }
        public void ViaNew() { new Foo().Run(); }
        public void ViaKeyword() { string.Join(\",\", \"a\"); }
    }
}
";
    let graph = graph(&[("Lib/Shop.cs", shop)]);
    let runs: Vec<String> = targets(&graph, "ViaNew")
        .into_iter()
        .filter(|target| target.ends_with(".Run"))
        .collect();
    assert_eq!(runs, ["Foo.Run"]);
    assert_eq!(targets(&graph, "ViaKeyword"), Vec::<String>::new());
}

/// A call whose own receiver call binds nothing (`Missing().Flush()`) falls
/// back to the untyped member: no edge to the caller's own `Flush`, which
/// HEAD bound.
#[test]
fn test_resolve_call_unknown_chain_receiver_binds_no_own_member() {
    let chain = "namespace Lib
{
    public class Chain
    {
        public void Flush() {}
        public void Use() { Missing().Flush(); }
    }
}
";
    assert_eq!(
        targets(&graph(&[("Lib/Chain.cs", chain)]), "Use"),
        Vec::<String>::new()
    );
}

/// Receivers next to the ruling's rows: an element, a cast, a `typeof`, a
/// string literal and a call inside `#if` are values of unknown type, so
/// none binds the caller's own `M` (HEAD bound it); `await typed.Run()`
/// keeps the typed binding.
#[test]
fn test_resolve_call_adjacent_value_receivers_bind_no_own_member() {
    let shapes = "using System.Threading.Tasks;
namespace Lib
{
    public class Shapes
    {
        private Shapes[] _all;
        public void M() {}
        public Task Run() { return null; }
        public void Use(object o)
        {
            _all[0].M();
            ((Shapes)o).M();
            typeof(Shapes).M();
            \"x\".M();
#if DEBUG
            o.M();
#endif
        }
        public async Task UseAwait(Shapes typed) { await typed.Run(); }
    }
}
";
    let graph = graph(&[("Lib/Shapes.cs", shapes)]);
    assert_eq!(targets(&graph, "Use"), Vec::<String>::new());
    assert_eq!(targets(&graph, "UseAwait"), ["Shapes.Run"]);
}

const WIDGET: (&str, &str) = (
    "Lib/Widget.cs",
    "namespace Lib { public class Widget {} public class Host { public Widget Current { get; } } }\n",
);

const EXT: (&str, &str) = (
    "Lib/WidgetExt.cs",
    "namespace Lib
{
    public static class WidgetExt
    {
        public static int Twice(this Widget w, int n) { return n; }
        private static int Hide(this Widget w) { return 0; }
    }
    public static class Util
    {
        public static int Plain(Widget w) { return 0; }
    }
}
",
);

const RUNNER: (&str, &str) = (
    "App/Runner.cs",
    "using Lib;
namespace App
{
    public class Runner : Host
    {
        private object Fetch() { return null; }
        public int One() { var x = Fetch(); return x.Twice(2); }
        public int NoArgs() { var x = Fetch(); return x.Twice(); }
        public int Hidden() { var x = Fetch(); return x.Hide(); }
        public int Static() { var x = Fetch(); return x.Plain(); }
        public int Inherited() { return Current.Twice(2); }
        public int Chained() { return Missing().Twice(2); }
        public int Typed(Widget w) { return w.Twice(2); }
    }
}
",
);

/// Codex ruling P-recv Q2: on an untyped receiver (`var x`, an inherited
/// property, a call that binds nothing) the one in-scope extension that
/// takes the arguments binds at 0.5 `call:extension-heuristic`. HEAD bound
/// the unique `Twice` at the global tier's confidence, reason `call`.
#[test]
fn test_resolve_call_untyped_receiver_single_extension_binds_heuristic() {
    let graph = graph(&[WIDGET, EXT, RUNNER]);
    for caller in ["One", "Inherited", "Chained"] {
        assert_eq!(
            calls_from(&graph, caller)
                .into_iter()
                .filter(|call| call.target == "WidgetExt.Twice")
                .map(|call| (call.confidence, call.reason))
                .collect::<Vec<_>>(),
            [(HEURISTIC_CONFIDENCE, HEURISTIC_REASON.to_string())],
            "{caller}"
        );
    }
}

/// The arguments refuse (`Twice()` lacks `n`), a private extension is out of
/// reach, and a plain static method is no extension: no edge. HEAD bound
/// the unique `Twice` and `Plain` by name.
#[test]
fn test_resolve_call_untyped_receiver_refused_extension_binds_nothing() {
    let graph = graph(&[WIDGET, EXT, RUNNER]);
    for caller in ["NoArgs", "Hidden", "Static"] {
        let bound: Vec<String> = targets(&graph, caller)
            .into_iter()
            .filter(|target| !target.ends_with(".Fetch"))
            .collect();
        assert_eq!(bound, Vec::<String>::new(), "{caller}");
    }
}

/// An extension in a namespace the caller does not import, and two
/// applicable extensions, bind nothing. HEAD bound the unique `Thrice`.
#[test]
fn test_resolve_call_untyped_receiver_out_of_scope_or_ambiguous_extension_binds_nothing() {
    let other = (
        "Other/OtherExt.cs",
        "namespace Other
{
    public static class OtherExt
    {
        public static int Thrice(this Lib.Widget w) { return 3; }
    }
}
",
    );
    let rival = (
        "Lib/Rival.cs",
        "namespace Lib
{
    public static class RivalExt
    {
        public static int Twice(this Widget w, int n) { return n; }
    }
}
",
    );
    let caller = (
        "App/Scope.cs",
        "using Lib;
namespace App
{
    public class Scope
    {
        private object Fetch() { return null; }
        public int OutOfScope() { var x = Fetch(); return x.Thrice(); }
        public int Ambiguous() { var x = Fetch(); return x.Twice(2); }
    }
}
",
    );
    let graph = graph(&[WIDGET, EXT, other, rival, caller]);
    for caller in ["OutOfScope", "Ambiguous"] {
        let bound: Vec<String> = targets(&graph, caller)
            .into_iter()
            .filter(|target| !target.ends_with(".Fetch"))
            .collect();
        assert_eq!(bound, Vec::<String>::new(), "{caller}");
    }
}

/// A proven extension on a typed receiver keeps 0.85 and the plain reason.
/// Guard: HEAD did the same.
#[test]
fn test_resolve_call_typed_receiver_proven_extension_keeps_owned_confidence() {
    let graph = graph(&[WIDGET, EXT, RUNNER]);
    let twice: Vec<(f32, String)> = calls_from(&graph, "Typed")
        .into_iter()
        .map(|call| (call.confidence, call.reason))
        .collect();
    assert_eq!(twice, [(0.85, "call".to_string())]);
}

/// Regression: a generic method call (`JsonConvert.DeserializeObject<Foo>(s)`,
/// `t.Value<int>(k)`) records its method name without the type arguments.
/// On HEAD the entry named `DeserializeObject<Foo>`, which no declaration
/// matches: 876 such sites on Newtonsoft.Json bound 69 edges.
#[test]
fn test_resolve_call_generic_method_call_binds_by_method_name() {
    let lib = "namespace Lib
{
    public static class JsonConvert
    {
        public static T DeserializeObject<T>(string value) { return default(T); }
    }
    public class Token
    {
        public T Value<T>(string key) { return default(T); }
    }
}
";
    let user = "namespace Lib
{
    public class Foo {}
    public class User
    {
        public void Use(Token t, string s)
        {
            Foo f = JsonConvert.DeserializeObject<Foo>(s);
            int n = t.Value<int>(\"k\");
        }
    }
}
";
    let graph = graph(&[("Lib/Lib.cs", lib), ("Lib/User.cs", user)]);
    assert_eq!(
        targets(&graph, "Use"),
        ["JsonConvert.DeserializeObject", "Token.Value"]
    );
}
