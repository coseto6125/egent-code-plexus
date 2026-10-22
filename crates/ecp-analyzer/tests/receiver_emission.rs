//! The call-site kind each parser hands the resolver for a member call.
//! The same-file pick (`SymbolTable::prefer_owned`) narrows a bare `m()` by
//! the caller's own type and bases, so a parser that writes `x.m()` as the
//! bare `m` binds the caller's own or inherited `m` to a call on an
//! unrelated receiver:
//!
//! - Swift and Ruby: an untyped receiver (`o.save()`, `o&.save`) emits an
//!   untyped member site, which no base narrows.
//! - Java: `new T().m()`, `((A) o).m()` emit `T.m` / `A.m`.
//! - Swift, Kotlin, Dart: an identifier receiver that no scope, import or
//!   enclosing type binds (`Util.make()`) emits a `FromReceiver` site that
//!   the resolver binds as a type. Python emits `Util.make` for a class of
//!   the same file.
//! - Swift: members of an `extension`, a protocol extension and an `enum`
//!   carry their type as owner, so they are no longer free functions to the
//!   same-file pick.
//!
//! Resolver dependencies:
//!
//! - `*_type_receiver_binds_type_member` (Swift, Kotlin, Dart) and the
//!   Kotlin extension-function guard need the resolver to read a Swift,
//!   Kotlin or Dart `FromReceiver` site as a C# one does.
//! - `*_binds_no_inherited_member` need the resolver to drop the caller's
//!   bases for an untyped member site in every tier, the heritage-scoped
//!   one included, not only in the same-file pick.
//!
//! An untyped `x.m()` in a type that declares its own `m` still binds that
//! `m`: the same-file pick keeps the caller's owner for every call site.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::CallSite;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

fn build(provider: &dyn LanguageProvider, path: &str, source: &str) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(
        provider
            .parse_file(Path::new(path), source.as_bytes())
            .unwrap(),
    );
    builder.build()
}

/// Owner classes of the `callee` nodes that the one node named `caller`
/// calls, sorted.
fn owners_called(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<String> {
    let pool = &graph.string_pool;
    let name = |i: u32| graph.nodes[i as usize].name.resolve(pool);
    let callers: Vec<u32> = (0..graph.nodes.len() as u32)
        .filter(|&i| name(i) == caller)
        .collect();
    assert_eq!(callers.len(), 1, "one `{caller}` node");
    let mut owners: Vec<String> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == callers[0])
        .filter(|e| name(e.target) == callee)
        .map(|e| {
            graph.nodes[e.target as usize]
                .owner_class
                .resolve(pool)
                .to_string()
        })
        .collect();
    owners.sort();
    owners
}

/// The raw `calls` entries of the one node named `caller`.
fn raw_calls(
    provider: &dyn LanguageProvider,
    path: &str,
    source: &str,
    caller: &str,
) -> Vec<String> {
    let local = provider
        .parse_file(Path::new(path), source.as_bytes())
        .unwrap();
    let mut found = local.nodes.iter().filter(|n| n.name == caller);
    let node = found.next().unwrap_or_else(|| panic!("no node `{caller}`"));
    assert!(found.next().is_none(), "one `{caller}` node");
    node.calls.clone()
}

/// `owner_class` of the one node named `name`.
fn owner_of(
    provider: &dyn LanguageProvider,
    path: &str,
    source: &str,
    name: &str,
) -> Option<String> {
    let local = provider
        .parse_file(Path::new(path), source.as_bytes())
        .unwrap();
    let mut found = local.nodes.iter().filter(|n| n.name == name);
    let node = found.next().unwrap_or_else(|| panic!("no node `{name}`"));
    assert!(found.next().is_none(), "one `{name}` node");
    node.owner_class.clone()
}

/// The node named `caller` calls `present` and none of `absent`.
fn assert_calls(
    provider: &dyn LanguageProvider,
    (path, source): (&str, &str),
    caller: &str,
    present: &str,
    absent: &[String],
) {
    let calls = raw_calls(provider, path, source, caller);
    assert!(
        calls.iter().any(|c| c == present),
        "{caller}: calls {present:?}; got {calls:?}"
    );
    for wrong in absent {
        assert!(
            !calls.contains(wrong),
            "{caller}: never calls {wrong:?}; got {calls:?}"
        );
    }
}

fn swift() -> ecp_analyzer::swift::parser::SwiftProvider {
    ecp_analyzer::swift::parser::SwiftProvider::new().unwrap()
}
fn ruby() -> ecp_analyzer::ruby::parser::RubyProvider {
    ecp_analyzer::ruby::parser::RubyProvider::new().unwrap()
}
fn java() -> ecp_analyzer::java::parser::JavaProvider {
    ecp_analyzer::java::parser::JavaProvider::new().unwrap()
}
fn kotlin() -> ecp_analyzer::kotlin::parser::KotlinProvider {
    ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap()
}
fn dart() -> ecp_analyzer::dart::parser::DartProvider {
    ecp_analyzer::dart::parser::DartProvider::new().unwrap()
}
fn python() -> ecp_analyzer::python::PythonProvider {
    ecp_analyzer::python::PythonProvider::new().unwrap()
}

// ─── Swift: untyped receivers ───────────────────────────────────────────────

/// `C` inherits `B.save`; the unrelated `A` declares `save` too. A call on
/// an untyped receiver reaches neither through `C`'s bases.
const SWIFT_UNTYPED: &str = r#"class A { func save() {} }
class B { func save() {} }
class C: B {
    var held = A()
    func make() -> A { return A() }
    func viaLocal() {
        let o = make()
        o.save()
    }
    func viaOptional() {
        let o = make()
        o?.save()
    }
    func viaProperty() {
        held.save()
    }
    func viaSelfProperty() {
        self.held.save()
    }
    func viaChain() {
        make().save()
    }
    func viaLabeledParam(for item: [A]) {
        item.save()
    }
    func viaImplicit() {
        save()
    }
    func viaSelf() {
        self.save()
    }
}
"#;

#[test]
fn test_swift_callee_name_untyped_receivers_bind_no_inherited_member() {
    let graph = build(&swift(), "C.swift", SWIFT_UNTYPED);
    for caller in [
        "viaLocal",
        "viaOptional",
        "viaProperty",
        "viaSelfProperty",
        "viaLabeledParam",
    ] {
        assert_eq!(
            owners_called(&graph, caller, "save"),
            Vec::<String>::new(),
            "{caller}: an untyped receiver binds no `save`"
        );
    }
    // `make() -> A` types the receiver of `make().save()`: a chained call
    // binds the member of the callee's declared return type.
    assert_eq!(owners_called(&graph, "viaChain", "save"), ["A"]);
}

#[test]
fn test_swift_callee_name_implicit_and_self_receivers_bind_inherited_member() {
    let graph = build(&swift(), "C.swift", SWIFT_UNTYPED);
    assert_eq!(owners_called(&graph, "viaImplicit", "save"), ["B"]);
    assert_eq!(owners_called(&graph, "viaSelf", "save"), ["B"]);
}

#[test]
fn test_swift_callee_name_untyped_receivers_emit_untyped_member() {
    let untyped = CallSite::untyped_member("save");
    for caller in [
        "viaLocal",
        "viaOptional",
        "viaProperty",
        "viaSelfProperty",
        "viaLabeledParam",
    ] {
        assert_calls(
            &swift(),
            ("C.swift", SWIFT_UNTYPED),
            caller,
            &untyped,
            &["save".to_string()],
        );
    }
    // A call receiver is a chain whose fallback is the same untyped member.
    let chain = raw_calls(&swift(), "C.swift", SWIFT_UNTYPED, "viaChain");
    assert!(
        chain
            .iter()
            .any(|c| CallSite::parse(c).fallback() == CallSite::UntypedMember("save")),
        "viaChain: got {chain:?}"
    );
    assert!(
        !chain.contains(&"save".to_string()),
        "viaChain: got {chain:?}"
    );
}

/// `$0` is a closure's implicit parameter, never a type.
#[test]
fn test_swift_callee_name_closure_shorthand_receiver_emits_untyped_member() {
    let src = "class C {\n    func run(xs: [Int]) {\n        xs.forEach {\n            $0.save()\n        }\n    }\n}\n";
    let local = swift()
        .parse_file(Path::new("C.swift"), src.as_bytes())
        .unwrap();
    let calls: Vec<&String> = local.nodes.iter().flat_map(|n| &n.calls).collect();
    assert!(
        calls.contains(&&CallSite::untyped_member("save")),
        "got {calls:?}"
    );
}

// ─── Swift: type-name receivers ─────────────────────────────────────────────

const SWIFT_TYPE: &str = r#"class Util {
    static func make() {}
}
class Other {}
class D {
    var Shadowed = Other()
    func make() {}
    func viaType() {
        Util.make()
    }
    func viaImplicit() {
        make()
    }
    func viaLocalNamedLikeType() {
        let Util = Other()
        Util.make()
    }
    func viaPropertyNamedLikeType() {
        Shadowed.make()
    }
}
"#;

#[test]
fn test_swift_callee_name_type_receiver_emits_from_receiver() {
    assert_eq!(
        raw_calls(&swift(), "D.swift", SWIFT_TYPE, "viaType"),
        [CallSite::from_receiver("Util", "make")]
    );
}

#[test]
fn test_swift_callee_name_local_or_property_named_like_type_emits_untyped_member() {
    for (caller, receiver) in [
        ("viaLocalNamedLikeType", "Util"),
        ("viaPropertyNamedLikeType", "Shadowed"),
    ] {
        assert_calls(
            &swift(),
            ("D.swift", SWIFT_TYPE),
            caller,
            &CallSite::untyped_member("make"),
            &[CallSite::from_receiver(receiver, "make")],
        );
    }
}

/// Needs the resolver to read a Swift `FromReceiver` site as a type.
#[test]
fn test_resolve_call_swift_type_receiver_binds_type_member() {
    let graph = build(&swift(), "D.swift", SWIFT_TYPE);
    assert_eq!(owners_called(&graph, "viaType", "make"), ["Util"]);
    assert_eq!(owners_called(&graph, "viaImplicit", "make"), ["D"]);
}

// ─── Swift: member owners ───────────────────────────────────────────────────

const SWIFT_OWNERS: &str = r#"class Outer {
    class Keyed {}
}
extension Outer.Keyed {
    func keyedMember() {}
}
protocol P {}
extension P {
    func protocolDefault() {}
}
enum Color {
    case red
    func describe() {}
}
struct Top {
    struct Nested {
        func nestedMember() {}
    }
}
extension Top {
    struct Inner {
        func innerMember() {}
    }
    func topExtensionMember() {}
}
extension Array where Element == Int {
    func arrayMember() {}
}
func freeFunction() {}
"#;

#[test]
fn test_swift_parse_extension_members_own_extended_type() {
    let owner = |name| owner_of(&swift(), "O.swift", SWIFT_OWNERS, name);
    assert_eq!(owner("keyedMember").as_deref(), Some("Keyed"));
    assert_eq!(owner("protocolDefault").as_deref(), Some("P"));
    assert_eq!(owner("topExtensionMember").as_deref(), Some("Top"));
    assert_eq!(owner("arrayMember").as_deref(), Some("Array"));
}

#[test]
fn test_swift_parse_enum_and_nested_type_members_own_their_type() {
    let owner = |name| owner_of(&swift(), "O.swift", SWIFT_OWNERS, name);
    assert_eq!(owner("describe").as_deref(), Some("Color"));
    assert_eq!(owner("nestedMember").as_deref(), Some("Nested"));
    assert_eq!(owner("innerMember").as_deref(), Some("Inner"));
    assert_eq!(owner("freeFunction"), None);
}

/// Alamofire's `URLEncodedFormEncoder.swift`: two extensions of nested
/// types each declare `encode`. An implicit-self `encode(..)` in the second
/// reaches its own `encode`, not the first one in source order.
#[test]
fn test_resolve_call_swift_extension_implicit_self_binds_own_extension_member() {
    let src = r#"class Outer {
    class Keyed {}
    class Single {}
}
extension Outer.Keyed {
    func encode(_ value: Int) {}
}
extension Outer.Single {
    func encode(_ value: Int) {}
    func encodeNil() {
        encode(1)
    }
}
"#;
    let graph = build(&swift(), "E.swift", src);
    assert_eq!(owners_called(&graph, "encodeNil", "encode"), ["Single"]);
}

// ─── Ruby ───────────────────────────────────────────────────────────────────

const RUBY_UNTYPED: &str = r#"class A
  def save
  end
end
class B
  def save
  end
end
class C < B
  def via_local(o)
    o.save
  end
  def via_safe_navigation(o)
    o&.save
  end
  def via_chain
    make.save
  end
  def via_implicit
    save()
  end
  def via_self
    self.save
  end
  def via_constant
    A.save
  end
end
"#;

/// sinatra's `base.rb`: `extensions_for(engine).each` bound the `each` of
/// `Helpers`, which `Base` includes.
#[test]
fn test_ruby_callee_untyped_receivers_bind_no_inherited_member() {
    let graph = build(&ruby(), "c.rb", RUBY_UNTYPED);
    for caller in ["via_local", "via_safe_navigation", "via_chain"] {
        assert_eq!(
            owners_called(&graph, caller, "save"),
            Vec::<String>::new(),
            "{caller}: an untyped receiver binds no `save`"
        );
    }
}

#[test]
fn test_ruby_callee_implicit_self_and_constant_receivers_bind_as_before() {
    let graph = build(&ruby(), "c.rb", RUBY_UNTYPED);
    assert_eq!(owners_called(&graph, "via_implicit", "save"), ["B"]);
    assert_eq!(owners_called(&graph, "via_self", "save"), ["B"]);
    assert_eq!(owners_called(&graph, "via_constant", "save"), ["A"]);
}

#[test]
fn test_ruby_callee_untyped_receivers_emit_untyped_member() {
    for caller in ["via_local", "via_safe_navigation", "via_chain"] {
        assert_calls(
            &ruby(),
            ("c.rb", RUBY_UNTYPED),
            caller,
            &CallSite::untyped_member("save"),
            &["save".to_string()],
        );
    }
}

// ─── Java ───────────────────────────────────────────────────────────────────

const JAVA_EXPR: &str = r#"class T { void m() {} }
class G<X> { void m() {} }
class Outer { class Inner { void m() {} } }
class A { void m() {} }
class D {
  void m() {}
  void viaNew() { new T().m(); }
  void viaGenericNew() { new G<String>().m(); }
  void viaAnonymous() { new T() {}.m(); }
  void viaInnerNew(Outer outer) { outer.new Inner().m(); }
  void viaCast(Object o) { ((A) o).m(); }
  void viaParenthesizedNew() { (new T()).m(); }
  void viaImplicit() { m(); }
}
"#;

/// guava's `TypeTokenTest.java`: `new From<Integer>().new To<String>().type()`
/// bound the caller's own `type`.
#[test]
fn test_java_callee_created_or_cast_receiver_binds_its_type() {
    let graph = build(&java(), "D.java", JAVA_EXPR);
    for (caller, owner) in [
        ("viaNew", "T"),
        ("viaGenericNew", "G"),
        ("viaAnonymous", "T"),
        ("viaInnerNew", "Inner"),
        ("viaCast", "A"),
        ("viaParenthesizedNew", "T"),
        ("viaImplicit", "D"),
    ] {
        assert_eq!(owners_called(&graph, caller, "m"), [owner], "{caller}");
    }
}

/// A qualified type (`new a.b.T()`, `(java.util.List<String>)`) names a
/// package path, which must not land on a project `T`: it stays untyped,
/// as a computed receiver does.
#[test]
fn test_java_callee_created_or_cast_receiver_emits_type_member() {
    let src = "class D {\n  void a() { new From<Integer>().new To<String>().type(); }\n  void b(Object o) { ((java.util.List<String>) o).size(); }\n  void c(Object o) { ((List<String>) o).size(); }\n  void d() { new a.b.T().m(); }\n  void e() { next().m(); }\n}\n";
    let has = |caller: &str, callee: &str| {
        raw_calls(&java(), "D.java", src, caller)
            .iter()
            .any(|c| c == callee)
    };
    assert!(has("a", "To.type"));
    assert!(has("b", &CallSite::untyped_member("size")));
    assert!(has("c", "List.size"));
    assert!(has("d", &CallSite::untyped_member("m")));
    // A call receiver is a chain the resolver types by the callee's return
    // type; untyped, it resolves as the same untyped member.
    assert!(raw_calls(&java(), "D.java", src, "e")
        .iter()
        .any(|c| CallSite::parse(c).fallback() == CallSite::UntypedMember("m")));
}

/// A computed receiver already emitted an untyped member site; the call
/// in `C` must not reach `B.save` through `C`'s bases either.
#[test]
fn test_resolve_call_java_computed_receiver_binds_no_inherited_member() {
    let src = "class A {\n  void save() {}\n}\nclass B {\n  void save() {}\n}\nclass C extends B {\n  Object next() {\n    return null;\n  }\n  void run() {\n    next().save();\n  }\n}\n";
    let graph = build(&java(), "C.java", src);
    assert_eq!(owners_called(&graph, "run", "save"), Vec::<String>::new());
}

/// A method reference is no call site: `T::m` emits nothing, as before.
#[test]
fn test_java_callee_method_reference_emits_no_call() {
    let src = "class D {\n  void a() { Runnable r = T::m; }\n}\n";
    assert_eq!(raw_calls(&java(), "D.java", src, "a"), Vec::<String>::new());
}

// ─── Kotlin ─────────────────────────────────────────────────────────────────

const KOTLIN_TYPE: &str = r#"class Util {
    fun make() {}
    companion object {
        fun create() {}
    }
}
object Reg {
    fun make() {}
}
class Other {
    fun make() {}
}
class D(val Param: Other) {
    val Holder = Other()
    fun make() {}
    fun create() {}
    fun viaCompanion() {
        Util.create()
    }
    fun viaObject() {
        Reg.make()
    }
    fun viaImplicit() {
        make()
    }
    fun viaPropertyNamedLikeType() {
        Holder.make()
    }
    fun viaConstructorParam() {
        Param.make()
    }
    fun viaLocalNamedLikeType() {
        val Reg = Other()
        Reg.make()
    }
    fun viaOptional(o: Other?) {
        o?.make()
    }
    fun viaChain() {
        Util.create().toString()
    }
}
"#;

#[test]
fn test_kotlin_callee_type_receiver_emits_from_receiver() {
    assert_eq!(
        raw_calls(&kotlin(), "D.kt", KOTLIN_TYPE, "viaCompanion"),
        [CallSite::from_receiver("Util", "create")]
    );
    assert_eq!(
        raw_calls(&kotlin(), "D.kt", KOTLIN_TYPE, "viaObject"),
        [CallSite::from_receiver("Reg", "make")]
    );
}

/// A property, constructor parameter or local named like a type is no type:
/// its call keeps the bare member, as before.
#[test]
fn test_kotlin_callee_property_param_or_local_named_like_type_emits_bare_member() {
    for (caller, receiver) in [
        ("viaPropertyNamedLikeType", "Holder"),
        ("viaConstructorParam", "Param"),
        ("viaLocalNamedLikeType", "Reg"),
        ("viaOptional", "o"),
    ] {
        assert_calls(
            &kotlin(),
            ("D.kt", KOTLIN_TYPE),
            caller,
            "make",
            &[CallSite::from_receiver(receiver, "make")],
        );
    }
}

#[test]
fn test_kotlin_callee_chained_call_keeps_outer_member_bare() {
    let calls = raw_calls(&kotlin(), "D.kt", KOTLIN_TYPE, "viaChain");
    // A chain on the call receiver, which resolves as the bare member when
    // the callee's return type does not type it.
    assert!(
        calls
            .iter()
            .any(|c| CallSite::parse(c).fallback() == CallSite::Plain("toString")),
        "got {calls:?}"
    );
    assert!(
        calls.contains(&CallSite::from_receiver("Util", "create")),
        "got {calls:?}"
    );
}

/// Needs the resolver to read a Kotlin `FromReceiver` site as a type.
#[test]
fn test_resolve_call_kotlin_type_receiver_binds_type_member() {
    let graph = build(&kotlin(), "D.kt", KOTLIN_TYPE);
    assert_eq!(owners_called(&graph, "viaCompanion", "create"), ["Util"]);
    assert_eq!(owners_called(&graph, "viaObject", "make"), ["Reg"]);
    assert_eq!(owners_called(&graph, "viaImplicit", "make"), ["D"]);
}

/// `fun Reg.ext()` is a top-level function, which no member lookup on `Reg`
/// finds: `Reg.ext()` keeps binding it, as the bare `ext` did.
#[test]
fn test_resolve_call_kotlin_extension_on_type_receiver_binds_top_level_function() {
    let src = "object Reg {\n    fun make() {}\n}\nfun Reg.ext() {}\nclass D {\n    fun ext() {}\n    fun go() {\n        Reg.ext()\n    }\n}\n";
    let graph = build(&kotlin(), "D.kt", src);
    assert_eq!(owners_called(&graph, "go", "ext"), [""]);
}

// ─── Dart ───────────────────────────────────────────────────────────────────

const DART_TYPE: &str = r#"import 'lib.dart' as p;
class Util {
  static void make() {}
}
class Other {
  void make() {}
}
class D {
  final Other Holder = Other();
  void make() {}
  void viaType() {
    Util.make();
  }
  void viaImplicit() {
    make();
  }
  void viaFieldNamedLikeType() {
    Holder.make();
  }
  void viaLocalNamedLikeType() {
    var Util = Other();
    Util.make();
  }
  void viaPrefix() {
    p.make();
  }
  void viaNullAware(Other o) {
    o?.make();
  }
}
"#;

#[test]
fn test_dart_callee_type_receiver_emits_from_receiver() {
    assert_eq!(
        raw_calls(&dart(), "d.dart", DART_TYPE, "viaType"),
        [CallSite::from_receiver("Util", "make")]
    );
}

/// A field, a local or an import prefix named like a type is no type: its
/// call keeps the bare member, as before.
#[test]
fn test_dart_callee_field_local_or_prefix_receiver_emits_bare_member() {
    for (caller, receiver) in [
        ("viaFieldNamedLikeType", "Holder"),
        ("viaLocalNamedLikeType", "Util"),
        ("viaPrefix", "p"),
    ] {
        assert_calls(
            &dart(),
            ("d.dart", DART_TYPE),
            caller,
            "make",
            &[CallSite::from_receiver(receiver, "make")],
        );
    }
}

/// `o?.make()` reads its typed receiver as `o.make()` does.
#[test]
fn test_dart_callee_null_aware_typed_receiver_emits_type_member() {
    assert_eq!(
        raw_calls(&dart(), "d.dart", DART_TYPE, "viaNullAware"),
        ["Other.make"]
    );
}

/// Needs the resolver to read a Dart `FromReceiver` site as a type.
#[test]
fn test_resolve_call_dart_type_receiver_binds_type_member() {
    let graph = build(&dart(), "d.dart", DART_TYPE);
    assert_eq!(owners_called(&graph, "viaType", "make"), ["Util"]);
    assert_eq!(owners_called(&graph, "viaImplicit", "make"), ["D"]);
}

// ─── Python ─────────────────────────────────────────────────────────────────

const PYTHON_TYPE: &str = r#"class Util:
    @staticmethod
    def make():
        pass

class D:
    def make(self):
        pass

    def via_type(self):
        Util.make()

    def via_implicit(self):
        self.make()

    def via_param_named_like_type(self, Util):
        Util.make()

    def via_local_named_like_type(self):
        Util = object()
        Util.make()
"#;

/// No resolver change: `Util.make` is the typed-receiver form.
#[test]
fn test_resolve_call_python_same_file_class_receiver_binds_class_member() {
    let graph = build(&python(), "d.py", PYTHON_TYPE);
    assert_eq!(owners_called(&graph, "via_type", "make"), ["Util"]);
    assert_eq!(owners_called(&graph, "via_implicit", "make"), ["D"]);
}

#[test]
fn test_python_callee_name_class_receiver_emits_qualified_member() {
    assert_eq!(
        raw_calls(&python(), "d.py", PYTHON_TYPE, "via_type"),
        ["Util.make"]
    );
}

#[test]
fn test_python_callee_name_param_or_local_named_like_class_emits_untyped_member() {
    for caller in ["via_param_named_like_type", "via_local_named_like_type"] {
        assert_calls(
            &python(),
            ("d.py", PYTHON_TYPE),
            caller,
            &CallSite::untyped_member("make"),
            &["Util.make".to_string()],
        );
    }
}

/// An untyped receiver in a subclass binds neither the unrelated `A.save`
/// nor the inherited `B.save`.
#[test]
fn test_resolve_call_python_untyped_receiver_binds_no_inherited_member() {
    let src = "class A:\n    def save(self):\n        pass\nclass B:\n    def save(self):\n        pass\nclass C(B):\n    def run(self, o):\n        o.save()\n";
    let graph = build(&python(), "c.py", src);
    assert_eq!(owners_called(&graph, "run", "save"), Vec::<String>::new());
}
