use crate::contracts::raw_records::{
    bind_enclosing, bind_name, EndpointRule, RawEndpoint, RawIdx, RecordKind, Unbound,
};
use crate::file_category::Language;
use crate::graph::NodeKind;
use rkyv::{Archive, Deserialize, Serialize};
use std::cell::Cell;
use std::path::PathBuf;

/// Source range as (start row, start column, end row, end column).
pub type Span = (u32, u32, u32, u32);

/// The lexical link from a callable to an anonymous closure it contains.
pub const CLOSURE_REFERENCE_REASON: &str = "closure:lexical_reference";

/// The link from the first of a same-name overload set to each later one
/// ([`crate::uid::keeps_overloads`]): a call binds the first by name only.
pub const OVERLOAD_SET_REASON: &str = "overload_set";

/// The confidence of a call edge bound to the nearest copy of a declaration
/// that duplicated source trees repeat. No other call tier yields this
/// value, so the index and the session overlay tell the edge apart by it.
pub const NEAREST_COPY_CONFIDENCE: f32 = 0.6;

/// The `reason` of a call edge at [`NEAREST_COPY_CONFIDENCE`]: path
/// proximity is a heuristic, so the edge stays tagged.
pub const NEAREST_COPY_REASON: &str = "call:nearest-copy";

/// True iff `outer` fully contains `inner` in the same coordinate system.
#[inline]
pub fn span_contains(outer: Span, inner: Span) -> bool {
    let (or1, oc1, or2, oc2) = outer;
    let (ir1, ic1, ir2, ic2) = inner;
    (or1, oc1) <= (ir1, ic1) && (ir2, ic2) <= (or2, oc2)
}

/// For each span, the index of the innermost other span that contains it.
///
/// Spans from one syntax tree nest or are disjoint. A sweep in start order
/// keeps open containers on a stack. Identical spans do not enclose each
/// other; the lower index wins between identical containers. Full indexing
/// and session overlays use this same rule, including same-line closures.
pub fn innermost_enclosing(spans: &[Span]) -> Vec<Option<usize>> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| {
        let (r1, c1, r2, c2) = spans[i];
        ((r1, c1), std::cmp::Reverse((r2, c2)), std::cmp::Reverse(i))
    });
    let mut parents = vec![None; spans.len()];
    let mut open: Vec<usize> = Vec::new();
    for i in order {
        while open
            .last()
            .is_some_and(|&top| !span_contains(spans[top], spans[i]))
        {
            open.pop();
        }
        parents[i] = open
            .iter()
            .rev()
            .find(|&&top| spans[top] != spans[i])
            .copied();
        open.push(i);
    }
    parents
}

/// How many receiver calls one chained entry nests. Without a bound, every
/// site of a chain thousands of calls long (`p.option().option()…`)
/// encodes, and stores, every call before it.
pub const CHAIN_DEPTH_LIMIT: u32 = 8;

thread_local! {
    static CHAIN_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// One [`RawNode::calls`] entry, decoded.
///
/// The callee text alone cannot say whether a call constructs its type: in
/// TS / JS / PHP / Java / C# only `new T()` constructs, and Python drops
/// an untyped receiver (`obj.widget()` and `widget()` both name
/// `widget`). The extractors that know encode the site with
/// [`CallSite::construct`] / [`CallSite::untyped_member`]. Each marker starts
/// with U+0001, a control character no callee text holds, so source text
/// such as JS `new Router().Handler` never decodes as a marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSite<'a> {
    /// `f()`, `obj.m()`, `A::m()`; in Python, Swift, Kotlin, Dart and C++
    /// also `A()`.
    Plain(&'a str),
    /// `new T(..)`: the site constructs `T`.
    Construct(&'a str),
    /// A member call whose receiver is neither typed nor rooted in an
    /// imported name (Python `obj.widget()`): it resolves by the member name
    /// and never constructs a type. A Rust method call on an untyped receiver
    /// (`n.name.resolve()`) uses it too, so it never reaches a free `fn`, and
    /// so does a PHP `$obj->m()` other than `$this`, which a `use function`
    /// import never names, a Java call on a computed receiver (`a.b().m()`),
    /// which no static import names, an untyped Swift, Ruby, C++ or Dart
    /// receiver, a JavaScript or TypeScript call or `new` receiver whose
    /// type stays unknown (`fetch(u).json()`), which no import or base
    /// class names, and a C# value whose type the parser cannot read
    /// (`var x`, a field of a generic type, `x?.m()`), which only an
    /// extension method may bind.
    UntypedMember(&'a str),
    /// A member call whose receiver may name a type. In Python the receiver
    /// is a name a `from` import binds (`from pkg import mod; mod.f()`): a
    /// submodule binds as a module import. In C#, Swift, Kotlin and Dart it
    /// is an identifier that is no local, parameter or field; in C# also the
    /// last segment of a dotted name that may be a type path (`Outer.Inner`)
    /// or a generic type's name (`Box<int>`). A receiver that names a type
    /// (or a library one) resolves as `Plain("Type.member")`; anywhere else
    /// the site resolves as `Plain(member)`, in Swift and C# as
    /// `UntypedMember(member)`.
    FromReceiver { receiver: &'a str, member: &'a str },
    /// A member call whose receiver is itself a call (`Foo.create().bar()`,
    /// `make().bar()`, `new Foo().bar()`). `inner` is the receiver call's own
    /// encoded entry, `fallback` the entry the parser wrote for this site
    /// before it recorded chains (a bare `bar` or an untyped member). The
    /// resolver types the receiver by the return type of the one function
    /// `inner` binds; where it cannot, the site resolves as `fallback`.
    ChainedMember { inner: &'a str, fallback: &'a str },
}

/// The arguments one call passes after its receiver, as the entry records
/// them ([`CallSite::with_arguments`]): the count of positional arguments
/// and the parameter names of the named ones. Only the C# parser records
/// them, on the member sites the extension retry reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallArguments<'a> {
    /// How many arguments name no parameter.
    pub positional: u32,
    /// The named arguments' parameter names, comma-joined, in source order.
    named: &'a str,
}

impl<'a> CallArguments<'a> {
    /// The parameter names the named arguments give, in source order.
    pub fn named(self) -> impl Iterator<Item = &'a str> {
        self.named.split(',').filter(|name| !name.is_empty())
    }
}

impl<'a> CallSite<'a> {
    const CONSTRUCT_PREFIX: &'static str = "\u{1}new ";
    const UNTYPED_MEMBER_PREFIX: &'static str = "\u{1}member ";
    const FROM_RECEIVER_PREFIX: &'static str = "\u{1}from ";
    const CHAINED_MEMBER_PREFIX: &'static str = "\u{1}chain ";
    /// Opens the argument record ahead of an entry; the chain separator
    /// closes it. It wraps the whole entry, so a chain's levels each carry
    /// their own record.
    const ARGUMENTS_PREFIX: &'static str = "\u{1}args ";
    /// The receiver of the outermost call [`Self::nested_chain`] cut. It
    /// names nothing; a chain that ends in it types none of its levels.
    pub const CUT_CHAIN: &'static str = "\u{1}cut";
    /// Ends the fallback of a chained entry. A fallback is never itself a
    /// chain, so the first separator ends it; `inner` may hold more.
    const CHAIN_SEPARATOR: char = '\u{2}';

    /// The qualifier of a Python `super().m()` callee (`super().m`): the
    /// resolver binds `m` on the caller class's bases, never by bare name.
    pub const SUPER_RECEIVER: &'static str = "super()";

    /// The `RawNode.calls` entry of a site that constructs `ty`.
    pub fn construct(ty: &str) -> String {
        [Self::CONSTRUCT_PREFIX, ty].concat()
    }

    /// The `RawNode.calls` entry of an untyped-receiver member call.
    pub fn untyped_member(member: &str) -> String {
        [Self::UNTYPED_MEMBER_PREFIX, member].concat()
    }

    /// The `RawNode.calls` entry of a member call on a `from`-imported name.
    pub fn from_receiver(receiver: &str, member: &str) -> String {
        [Self::FROM_RECEIVER_PREFIX, receiver, ".", member].concat()
    }

    /// Runs `encode`, which encodes the receiver call of a chained site, one
    /// nesting level deeper. Past [`CHAIN_DEPTH_LIMIT`] levels it yields
    /// [`Self::CUT_CHAIN`], and [`Self::resolve_chain`] resolves the site as
    /// its fallback.
    ///
    /// The depth is thread-local because each parser's encoder recurses
    /// through its own signature. It is restored on drop, so an encoder that
    /// unwinds leaves no raised depth for the thread's next file.
    pub fn nested_chain(encode: impl FnOnce() -> Option<String>) -> Option<String> {
        struct Restore(u32);
        impl Drop for Restore {
            fn drop(&mut self) {
                CHAIN_DEPTH.set(self.0);
            }
        }
        let level = CHAIN_DEPTH.get();
        if level >= CHAIN_DEPTH_LIMIT {
            return Some(Self::CUT_CHAIN.to_owned());
        }
        CHAIN_DEPTH.set(level + 1);
        let _restore = Restore(level);
        encode()
    }

    /// The `RawNode.calls` entry of a member call on the call `inner`
    /// (itself an encoded entry), which resolves as `fallback` when the
    /// receiver's type stays unknown.
    pub fn chained_member(inner: &str, fallback: &str) -> String {
        let mut raw = String::with_capacity(
            Self::CHAINED_MEMBER_PREFIX.len() + fallback.len() + 1 + inner.len(),
        );
        raw.push_str(Self::CHAINED_MEMBER_PREFIX);
        raw.push_str(fallback);
        raw.push(Self::CHAIN_SEPARATOR);
        raw.push_str(inner);
        raw
    }

    /// `entry` with the arguments of its call recorded ahead of it:
    /// `positional` unnamed arguments, then one named argument per name in
    /// `named`. [`Self::parse`] decodes the result as it decodes `entry`.
    pub fn with_arguments(entry: &str, positional: u32, named: &[&str]) -> String {
        use std::fmt::Write;
        let mut raw = String::with_capacity(Self::ARGUMENTS_PREFIX.len() + 4 + entry.len());
        raw.push_str(Self::ARGUMENTS_PREFIX);
        let _ = write!(raw, "{positional}");
        for name in named {
            raw.push(',');
            raw.push_str(name);
        }
        raw.push(Self::CHAIN_SEPARATOR);
        raw.push_str(entry);
        raw
    }

    /// The arguments the entry `raw` records ([`Self::with_arguments`]).
    /// `None` for an entry that records none: every non-C# entry, and a C#
    /// site whose argument list the parser does not model.
    pub fn arguments(raw: &'a str) -> Option<CallArguments<'a>> {
        let (spec, _) = Self::split_arguments(raw)?;
        let (count, named) = spec.split_once(',').unwrap_or((spec, ""));
        Some(CallArguments {
            positional: count.parse().ok()?,
            named,
        })
    }

    /// `(argument record, entry)` of an entry that records its arguments.
    fn split_arguments(raw: &'a str) -> Option<(&'a str, &'a str)> {
        raw.strip_prefix(Self::ARGUMENTS_PREFIX)?
            .split_once(Self::CHAIN_SEPARATOR)
    }

    pub fn parse(raw: &'a str) -> Self {
        // The argument record is no part of the site: an entry decodes as
        // it did before the parser recorded arguments.
        let raw = Self::split_arguments(raw).map_or(raw, |(_, entry)| entry);
        if let Some((fallback, inner)) = raw
            .strip_prefix(Self::CHAINED_MEMBER_PREFIX)
            .and_then(|site| site.split_once(Self::CHAIN_SEPARATOR))
        {
            Self::ChainedMember { inner, fallback }
        } else if let Some(ty) = raw.strip_prefix(Self::CONSTRUCT_PREFIX) {
            Self::Construct(ty)
        } else if let Some(member) = raw.strip_prefix(Self::UNTYPED_MEMBER_PREFIX) {
            Self::UntypedMember(member)
        } else if let Some((receiver, member)) = raw
            .strip_prefix(Self::FROM_RECEIVER_PREFIX)
            .and_then(|site| site.split_once('.'))
        {
            Self::FromReceiver { receiver, member }
        } else {
            Self::Plain(raw)
        }
    }

    /// The site a [`Self::ChainedMember`] resolves as when its receiver
    /// stays untyped; any other site is its own fallback.
    pub fn fallback(self) -> Self {
        match self {
            Self::ChainedMember { fallback, .. } => Self::parse(fallback),
            site => site,
        }
    }

    /// Resolve the entry `raw` with `resolve`, and a chain (`a().b().c()`)
    /// from its innermost call outwards: each level takes `typed_member` of
    /// the targets the level below bound, or else resolves its
    /// [`Self::fallback`]. A site that is no chain is one `resolve` call.
    /// Each site and level comes with the arguments its own entry records
    /// ([`Self::arguments`]); a level that falls back keeps them. The index
    /// and the query-time overlay both walk chains here, so a chain binds
    /// the same on both sides.
    pub fn resolve_chain<T>(
        raw: &'a str,
        mut resolve: impl FnMut(Self, Option<CallArguments<'a>>) -> T,
        mut typed_member: impl FnMut(&T, &'a str, Option<CallArguments<'a>>) -> Option<T>,
    ) -> T {
        let top = (Self::parse(raw), Self::arguments(raw));
        let mut levels: Vec<(Self, Option<CallArguments<'a>>)> = Vec::new();
        let mut innermost = top;
        while let (Self::ChainedMember { inner, .. }, _) = innermost {
            levels.push(innermost);
            innermost = (Self::parse(inner), Self::arguments(inner));
        }
        // A cut chain lost its root: typing its levels through their
        // fallbacks could bind a namesake, so the site resolves as it did
        // before chains.
        if innermost.0 == Self::Plain(Self::CUT_CHAIN) {
            return resolve(top.0.fallback(), top.1);
        }
        let mut targets = resolve(innermost.0, innermost.1);
        for (level, arguments) in levels.into_iter().rev() {
            // A fallback may be the whole callee text (the JS parser writes
            // `new Foo().bar`); the typed member is its last segment.
            let name = level.name();
            let member = name.rsplit(['.', ':']).next().unwrap_or(name);
            targets = match typed_member(&targets, member, arguments) {
                Some(typed) => typed,
                None => resolve(level.fallback(), arguments),
            };
        }
        targets
    }

    /// The callee the callable tiers resolve.
    pub fn name(self) -> &'a str {
        match self {
            Self::Plain(name)
            | Self::Construct(name)
            | Self::UntypedMember(name)
            | Self::FromReceiver { member: name, .. } => name,
            Self::ChainedMember { .. } => self.fallback().name(),
        }
    }

    /// Whether the call uses member syntax rather than a module path.
    pub fn uses_method_syntax(self) -> bool {
        match self {
            Self::UntypedMember(_) | Self::FromReceiver { .. } | Self::ChainedMember { .. } => true,
            Self::Plain(name) => name.contains('.'),
            Self::Construct(_) => false,
        }
    }

    /// Whether the site is a bare `f()` in a language where a call written with no
    /// receiver never reaches a member: PHP needs `$this->` / `self::`, Python
    /// `self.`, Rust `self.` / `Self::`, and C declares no members. A same-named
    /// method of an unrelated class (a Laravel test's private `config()` for the
    /// global helper `config()`, the caller's own or its base's `config`) must
    /// not bind it at any tier (the resolver's `CallScope::free_only`). Java,
    /// Kotlin, C#, Swift, Dart, Scala, C++ and Ruby call a member of the
    /// enclosing type with no receiver. JavaScript, TypeScript and Go also require
    /// an explicit receiver. Their parsers encode retained member fallbacks as a
    /// member site rather than `Plain`, so a genuine bare call stays free-only.
    pub fn reaches_free_only(self, language: Language) -> bool {
        matches!(
            language,
            Language::Php
                | Language::Python
                | Language::Rust
                | Language::C
                | Language::JavaScript
                | Language::TypeScript
                | Language::Go
        ) && matches!(self, Self::Plain(name) if !name.contains(['.', ':']))
    }

    /// The type path this site may construct in `language`, and whether an
    /// unresolved qualified path may fall back to its last segment. `None`
    /// when the site cannot construct a type in `language`: Go, Rust and C
    /// have no constructors, and in TS / JS / PHP / Java / C# only `new T()`
    /// constructs.
    ///
    /// The fallback holds only where the site constructs for certain and its
    /// qualifier can name what the type tiers do not resolve: a TS / JS
    /// namespace import (`new shop.Widget()`), a PHP or C++ namespace, a Ruby
    /// `Shop::Item.new`. A possible construction (`pkg.A()`) never falls
    /// back, and neither does a Java / C# `new`: its qualifier is a package
    /// path, and `new java.util.Date()` must not land on a project `Date`.
    pub fn constructed_type(self, language: Language) -> Option<(&'a str, bool)> {
        match (language, self) {
            (Language::Ruby, Self::Plain(callee)) => {
                callee.strip_suffix(".new").map(|ty| (ty, true))
            }
            (
                Language::TypeScript | Language::JavaScript | Language::Php | Language::Cpp,
                Self::Construct(ty),
            ) => Some((ty, true)),
            (Language::Java | Language::CSharp, Self::Construct(ty)) => Some((ty, false)),
            (
                Language::Python
                | Language::Swift
                | Language::Kotlin
                | Language::Dart
                | Language::Cpp,
                Self::Plain(callee),
            ) => Some((callee, false)),
            (Language::Python, Self::FromReceiver { member, .. }) => Some((member, false)),
            // Only the Python parser emits a `from` receiver.
            (_, Self::FromReceiver { .. }) => None,
            (_, Self::ChainedMember { .. }) => self.fallback().constructed_type(language),
            _ => None,
        }
    }
}

/// The key that names an owning type in an `owner_class` string: the last
/// `.` / `::` / `\` segment with generic arguments cut, trimmed. Parsers
/// mostly emit a bare name (`Dog`), but C++ out-of-line definitions, Ruby
/// `class A::B` and generic impls can carry a path or `<T>`. Collapsing to
/// the last segment loses no identity: every owned lookup is also scoped to
/// one file or one crate root, and the caller treats several hits as
/// ambiguous. `None` for an empty owner (Rust inherent `impl` blocks).
///
/// The index and the query-time overlay both compare owners through this
/// function, so a type's members are the same set on both sides.
pub fn owner_key(raw: &str) -> Option<&str> {
    let mut depth = 0u32;
    let mut seg_start = 0;
    let mut seg_end = None;
    for (i, c) in raw.char_indices() {
        match c {
            '<' | '[' => {
                if depth == 0 && seg_end.is_none() {
                    seg_end = Some(i);
                }
                depth += 1;
            }
            '>' | ']' => depth = depth.saturating_sub(1),
            '.' | ':' | '\\' if depth == 0 => {
                seg_start = i + c.len_utf8();
                seg_end = None;
            }
            _ => {}
        }
    }
    let key = raw[seg_start..seg_end.unwrap_or(raw.len())].trim();
    (!key.is_empty()).then_some(key)
}

/// Language-agnostic function metadata captured during parsing, before the
/// string pool is available. Stored in `LocalGraph` and converted to
/// `FunctionMeta` (with interned `StrRef`s) by `GraphBuilder::build`.
///
/// Keyed by `span` — must match the `RawNode.span` of the function/method/
/// constructor it describes. The builder pairs them via span lookup.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawFunctionMeta {
    /// Span of the corresponding `RawNode` (used as the pairing key).
    pub span: (u32, u32, u32, u32),
    /// Bit-packed flags using the same layout as `FunctionMeta::flags`.
    /// Callers use `FunctionMeta::FLAG_*` constants and the 3-bit visibility
    /// shift (bits 6-8) to build this value.
    pub flags: u16,
    /// Flat alternating `[name1, type1, name2, type2, ...]` — empty String
    /// for absent type annotations (dynamic-typed languages).
    pub params: Vec<String>,
    /// Return type as written in source. Empty string when absent/void.
    pub return_type: String,
    /// Decorator/attribute names in source order (e.g. `"staticmethod"`,
    /// `"#[test]"`, `"@Injectable"`).
    pub decorators: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct IdentifierRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub row: usize,
    pub col: usize,
}

/// One symbol as a language parser saw it, before cross-file resolution.
///
/// NOTE: function-level flags (`is_static` / `is_async` / `visibility` /
/// `is_extern` …) do NOT live here — they are extracted by the per-language
/// `function_meta/<lang>.rs` pass into a span-keyed side-table and merged onto
/// the graph `Node` at build time. "RawNode has no such field" therefore does
/// NOT mean the schema lacks the concept; check `function_meta` first (this
/// exact misread has produced two wrong audit conclusions).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawNode {
    pub name: String,
    pub kind: NodeKind,
    pub span: (u32, u32, u32, u32),
    pub is_exported: bool,
    pub heritage: Vec<String>, // Base classes, interfaces, traits
    pub type_annotation: Option<String>,
    pub decorators: Vec<String>,
    /// Names of functions/methods invoked from inside this node's body.
    /// Each entry is the callee's *short* name (e.g. `method` for `obj.method()`).
    /// Resolved against imports + same-file symbols in Pass 2 → `RelType::Calls`.
    /// An entry may carry a [`CallSite`] prefix; read entries through
    /// [`CallSite::parse`].
    pub calls: Vec<String>,
    /// Short names of struct/class fields read inside this node's body (e.g.
    /// `rel_path` for `obj.rel_path`). Resolved against imports + same-file
    /// symbols in Pass 2 → `RelType::ReadsField`. Only names that resolve to a
    /// `NodeKind::Property` target emit an edge, so method-call and local-var
    /// reads parsers may over-capture here are dropped at resolution.
    /// Marked entries are [`VariantRef`]s and [`NameRef`]s instead.
    pub field_reads: Vec<String>,
    /// Owning class/struct/trait for methods and properties.
    /// Set by each language parser at parse time; `None` for module-level
    /// functions. Eliminates the need for post-process span containment to
    /// establish class membership — parsers have grammar-level access to the
    /// enclosing type that the post-pass must re-derive from spans.
    pub owner_class: Option<String>,
    /// xxh3-64 hash of the symbol's raw source bytes
    /// (`source[start_byte..end_byte]` from the tree-sitter node).
    /// Computed by each language parser at parse time. `0` for synthetic
    /// nodes that have no corresponding source span (e.g. delegate stubs).
    /// Used by T7-4/5/6 incremental indexers to detect unchanged symbols
    /// without re-parsing.
    pub content_hash: u64,
}

/// The [`RawNode::decorators`] entry prefix that records a type's or a
/// delegate's declared generic arity (`Conv<T>` is 1, `delegate R
/// Func<T, R>(T a)` is 2). Only the C# parser writes it, and only for an
/// arity above 0. It rides in `decorators`, beside the
/// `__override__` marker, so the parse cache and every `RawNode` builder
/// keep their shape; `post_process::decorates_edges` skips it.
const GENERIC_ARITY_MARKER: &str = "__generic_arity__:";

/// The [`RawNode::decorators`] entry prefix that marks a C# type declared
/// `partial`, followed by the scope that declares it: the namespace, then
/// each enclosing type as `+Name`, with a backtick and the generic arity
/// when that is above 0 (`Lib.Json+Outer`1`). The builder merges the parts
/// of one type by this scope (`c_sharp::partial`). Only the C# parser
/// writes it, beside [`GENERIC_ARITY_MARKER`], and
/// `post_process::decorates_edges` skips it.
const PARTIAL_MARKER: &str = "__partial__:";

/// The [`RawNode::decorators`] entry the Kotlin, C#, C++ and Swift parsers
/// write when a method carries the `override` keyword. The Overrides
/// post-process reads it; it is not a decorator the source wrote.
pub const OVERRIDE_SENTINEL: &str = "__override__";

impl RawNode {
    /// The [`RawNode::decorators`] entry for a `partial` type declared in
    /// `scope`.
    pub fn partial_marker(scope: &str) -> String {
        format!("{PARTIAL_MARKER}{scope}")
    }

    /// Whether a decorator entry is a [`RawNode::partial_marker`].
    pub fn is_partial_marker(decorator: &str) -> bool {
        decorator.starts_with(PARTIAL_MARKER)
    }

    /// Whether a decorator entry is any parser marker rather than a
    /// decorator the source wrote: the [`OVERRIDE_SENTINEL`], a
    /// [`RawNode::generic_arity_marker`] or a [`RawNode::partial_marker`].
    pub fn is_parser_marker(decorator: &str) -> bool {
        decorator == OVERRIDE_SENTINEL
            || Self::is_generic_arity_marker(decorator)
            || Self::is_partial_marker(decorator)
    }

    /// The declaring scope of a `partial` type, as the parser recorded it;
    /// `None` when the node carries no partial marker.
    pub fn partial_scope(&self) -> Option<&str> {
        self.decorators
            .iter()
            .find_map(|d| d.strip_prefix(PARTIAL_MARKER))
    }

    /// The [`RawNode::decorators`] entry for a declared generic arity.
    pub fn generic_arity_marker(arity: u32) -> String {
        format!("{GENERIC_ARITY_MARKER}{arity}")
    }

    /// Whether a decorator entry is a [`RawNode::generic_arity_marker`].
    pub fn is_generic_arity_marker(decorator: &str) -> bool {
        decorator.starts_with(GENERIC_ARITY_MARKER)
    }

    /// The declared generic arity the parser recorded: 0 when the node
    /// carries no marker.
    pub fn generic_arity(&self) -> u32 {
        self.decorators
            .iter()
            .find_map(|d| d.strip_prefix(GENERIC_ARITY_MARKER)?.parse().ok())
            .unwrap_or(0)
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum BindingKind {
    /// Symbol → symbol (typedef, identifier-bodied `#define`, extern decls).
    Alias,
    /// Symbol → literal value (`#define MAX 4096`, `#define VER "v1"`).
    Constant,
    /// Symbol → expression (function-like `#define ADD(a,b)`, parenthesized expressions).
    Macro,
    /// Empty body, non-guard (`#define DEBUG`, `#define ENABLE_FOO`).
    Flag,
    /// TS / JS default import (`import d from './m'`). The parsers emit it
    /// as `imported_name` `d` with no alias, like `import { d } from './m'`,
    /// but `d` binds module `m`'s default export, whatever its declared name.
    EsDefault,
    /// A TS / JS binding the module exports: a re-export (`export { x } from
    /// './m'`), which the parsers emit like `import { x } from './m'`, or an
    /// import the module's own `export { x }` clause exports under its name.
    /// A plain import exports nothing, so another module's import of the
    /// file never reaches through it.
    EsReexport,
    /// An [`Self::EsDefault`] import the module also exports, as
    /// [`Self::EsReexport`] says: `import d from './m'; export { d };`.
    EsDefaultReexport,
}

impl From<&ArchivedBindingKind> for BindingKind {
    fn from(archived: &ArchivedBindingKind) -> Self {
        match archived {
            ArchivedBindingKind::Alias => Self::Alias,
            ArchivedBindingKind::Constant => Self::Constant,
            ArchivedBindingKind::Macro => Self::Macro,
            ArchivedBindingKind::Flag => Self::Flag,
            ArchivedBindingKind::EsDefault => Self::EsDefault,
            ArchivedBindingKind::EsReexport => Self::EsReexport,
            ArchivedBindingKind::EsDefaultReexport => Self::EsDefaultReexport,
        }
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawImport {
    pub source: String,
    /// Empty when the dependency is known but its symbol visibility is unmodeled.
    /// Dart imports with `show` or `hide` retain only the dependency source.
    pub imported_name: String,
    pub alias: Option<String>,
    /// `None` for ordinary import statements; `Some(_)` for C named bindings
    /// (`typedef`, `#define`, `extern`) classified by body shape, and for a
    /// TS / JS default import or exported binding ([`BindingKind::EsDefault`],
    /// [`BindingKind::EsReexport`], [`BindingKind::EsDefaultReexport`]).
    pub binding_kind: Option<BindingKind>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawRoute {
    pub method: String,
    pub path: String,
    /// What serves the route (C4 `RecordKind::Route`). A producer writes
    /// `Unresolved(Named)` for a handler the source names and
    /// `Unresolved(Inline)` at the handler argument's span for a function
    /// written at the registration; [`Self::bind`] turns either into `Node`
    /// when one node of the file is that handler. `None`: the producer saw
    /// no handler.
    pub handler: Option<RawEndpoint>,
    pub span: (u32, u32, u32, u32),
    /// What the enclosing route groups apply (Laravel `Route::group`,
    /// `->group(...)`), and, for a [`RawRoute::INCLUDE`] record, the route
    /// file the group loads. `None` for a provider without route groups.
    pub scope: Option<Box<RawRouteScope>>,
}

/// The kinds a route handler binds to: code, or an invokable class.
const ROUTE_HANDLER: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Constructor,
    NodeKind::Class,
];

/// The kinds whose body holds statements, so a site inside one is a call,
/// never a decorator.
const CALLABLE_KINDS: &[NodeKind] = &[NodeKind::Function, NodeKind::Method, NodeKind::Constructor];

impl RawRoute {
    /// `method` of a route-file include record, such as
    /// `Route::prefix('admin')->group(base_path('routes/admin.php'))`. It is
    /// not an endpoint: `path` holds the group prefix (empty for none), and
    /// the builder applies the record's prefix, middleware and namespace to
    /// every route of the included file. `handler` names the group's
    /// controller (`Route::controller(C::class)->group(...)`), whose method a
    /// bare action string in the included file names.
    pub const INCLUDE: &'static str = "INCLUDE";

    /// `method` of a Laravel controller-middleware record. It is not an
    /// endpoint: the builder adds its middleware to each route whose action
    /// binds a method of the class that declares it. `path` says where the
    /// class registers it: `__construct` (`$this->middleware('auth')`),
    /// `middleware` (an entry of the static `HasMiddleware::middleware()`),
    /// or `parent::__construct` for a constructor that runs its parent's,
    /// which carries no middleware. `__construct?` and `middleware?` mark a
    /// registration the parser cannot read (reason after a space), and
    /// `extends` / `implements` / `use` carry the class's parent, interfaces
    /// and traits (fully qualified name after a space).
    /// `scope.only` / `scope.except` hold the method filter.
    pub const CONTROLLER_MIDDLEWARE: &'static str = "CONTROLLER_MIDDLEWARE";

    /// A handler the source names: `None` for an empty or whitespace-only
    /// name, as for no name.
    pub fn named_handler(name: Option<&str>) -> Option<RawEndpoint> {
        name.and_then(RawEndpoint::named)
    }

    /// The handler's name as the source writes it, while no node binds it.
    pub fn handler_name(&self) -> Option<&str> {
        match &self.handler {
            Some(RawEndpoint::Unresolved(Unbound::Named(name))) => Some(name.as_str()),
            _ => None,
        }
    }

    /// The handler once the file's node list is final.
    ///
    /// | `handler` | Result |
    /// |---|---|
    /// | `Named`, record without a group scope | the node [`bind_name`] binds from the site; else the declaration a decorator at the site decorates, when it has that name; else `Named` |
    /// | `Named`, record with a group scope | `Named`: a Laravel action binds across files, after heritage |
    /// | `Inline(at)` | the outermost `<anonymous:` closure node that starts where the handler argument `at` starts and lies inside it; else `Inline(at)` |
    /// | `Anonymous`, `Node`, `File`, `None` | unchanged |
    ///
    /// An include or controller-middleware record is never bound. Binding
    /// twice gives the same endpoint.
    pub fn bind(&self, nodes: &[RawNode]) -> Option<RawEndpoint> {
        if self.method == Self::INCLUDE || self.method == Self::CONTROLLER_MIDDLEWARE {
            return self.handler.clone();
        }
        match &self.handler {
            Some(RawEndpoint::Unresolved(Unbound::Named(name))) if self.scope.is_none() => {
                Some(bind_route_handler(nodes, self.span, name.as_str()))
            }
            Some(RawEndpoint::Unresolved(Unbound::Inline(at))) => {
                Some(route_closure(nodes, *at).map_or(
                    RawEndpoint::Unresolved(Unbound::Inline(*at)),
                    RawEndpoint::Node,
                ))
            }
            other => other.clone(),
        }
    }
}

/// A handler named at `site`.
///
/// Outside every callable, the site can be a decorator: the declaration
/// that starts first after it is the one it decorates (`@Get(':id')
/// find()`, `@app.get("/x") def find()`), and binds when it has the name.
/// A class member or a same-name declaration elsewhere in the file never
/// takes its place. Otherwise the handler is the one node [`bind_name`]
/// binds; two visible candidates bind neither.
fn bind_route_handler(nodes: &[RawNode], site: Span, name: &str) -> RawEndpoint {
    if bind_enclosing(nodes, site, CALLABLE_KINDS).is_none() {
        let next = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| (n.span.0, n.span.1) >= (site.2, site.3))
            .min_by_key(|(i, n)| {
                let (r1, c1, r2, c2) = n.span;
                ((r1, c1), std::cmp::Reverse((r2, c2)), *i)
            });
        if let Some((i, n)) = next {
            if n.name == name && ROUTE_HANDLER.contains(&n.kind) {
                if let Some(idx) = RawIdx::checked(i, nodes.len()) {
                    return RawEndpoint::Node(idx);
                }
            }
        }
    }
    bind_name(nodes, site, name, ROUTE_HANDLER)
        .endpoint()
        .clone()
}

/// The closure node that the handler argument at `at` became: the outermost
/// `<anonymous:` `Function` node that starts where the argument starts and
/// lies inside it. A middleware closure of the same registration starts
/// elsewhere, and a closure nested in the handler's body starts later, so
/// neither takes the handler's place. A handler with no node of its own (a
/// parser emits none for an empty body) binds nothing.
fn route_closure(nodes: &[RawNode], at: Span) -> Option<RawIdx> {
    let (i, _) = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            n.kind == NodeKind::Function
                && n.name.starts_with("<anonymous:")
                && (n.span.0, n.span.1) == (at.0, at.1)
                && span_contains(at, n.span)
        })
        .max_by_key(|&(i, n)| ((n.span.2, n.span.3), std::cmp::Reverse(i)))?;
    RawIdx::checked(i, nodes.len())
}

/// The group scope of one [`RawRoute`], as far as its own file shows it.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Default, PartialEq)]
#[rkyv(derive(Debug))]
pub struct RawRouteScope {
    /// Middleware in effect, the outermost group's first and the route's
    /// own last.
    pub middleware: Vec<String>,
    /// Controller namespace in effect for a relative class name in
    /// `handler`. A leading `\` marks a namespace that an including group's
    /// namespace does not prefix. `None` when no group in the file sets one.
    pub namespace: Option<String>,
    /// The included route file of a [`RawRoute::INCLUDE`] record.
    pub include: Option<RouteInclude>,
    /// The file imports no HTTP framework, as a route file that calls the
    /// global `Route` facade alias does. Its records count only when a file
    /// that imports one loads it, directly or through other route files.
    pub needs_include: bool,
    /// Middleware removed again: the route's `->withoutMiddleware('auth')`,
    /// a group's `Route::withoutMiddleware(...)` or `'excluded_middleware'`
    /// option. Laravel drops each name from the composed list, controller
    /// middleware included, by exact string match.
    pub excluded_middleware: Vec<String>,
    /// The controller methods a [`RawRoute::CONTROLLER_MIDDLEWARE`] record
    /// applies to (`['only' => [...]]`, `->only(...)`); `None` for every
    /// method.
    pub only: Option<Vec<String>>,
    /// The controller methods a [`RawRoute::CONTROLLER_MIDDLEWARE`] record
    /// skips (`['except' => [...]]`, `->except(...)`).
    pub except: Option<Vec<String>>,
}

/// The route file a Laravel route group loads.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug))]
pub enum RouteInclude {
    /// `base_path('routes/api.php')`: relative to the application root.
    AppRoot(String),
    /// `__DIR__ . '/api.php'`: relative to the including file's directory,
    /// `..` segments kept.
    FileDir(String),
}

/// Path-shaped string literal extracted by a per-language `path_literals`
/// walker. Promoted by `post_process::path_literal_nodes` into a
/// `NodeKind::PathLiteral` plus a `UsesPathLiteral` edge from its
/// [`Self::endpoint`]: the innermost enclosing callable or `Const`, else the
/// file (C4 `RecordKind::PathLiteral`).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawPathLiteral {
    /// The literal value, post quote-stripping (e.g. `session_meta.json`).
    pub value: String,
    /// Span of the literal in source.
    pub span: (u32, u32, u32, u32),
    /// Resolved name of the enclosing Function/Method/Const when one exists.
    /// `None` if the literal is at module top-level (Python global, JS top-level,
    /// Rust `const X: &str = "…";` is captured as Const-owner via the other
    /// field).
    pub enclosing_symbol: Option<String>,
    /// Owner class of the enclosing symbol when it's a method. Lets the
    /// post-process pass disambiguate `Foo::method` from `Bar::method` when
    /// both define the same method name in the same file.
    pub enclosing_owner: Option<String>,
    /// Pre-rendered sink reason payload, generated by
    /// `ecp_analyzer::path_literal::sink_reason`. Example:
    /// `sink:read|confidence:high`. Consumed verbatim by the post-process
    /// emitter into `Edge.reason`.
    pub sink_reason: String,
    /// The node or file the literal attaches to, written by
    /// [`LocalGraph::bind_records`] once the node list is final. `None`
    /// before binding.
    pub endpoint: Option<RawEndpoint>,
}

impl RawPathLiteral {
    pub fn bind(&self, nodes: &[RawNode]) -> Option<RawEndpoint> {
        RecordKind::PathLiteral.bind_site(nodes, self.span)
    }

    /// [`Self::endpoint`], or the one [`LocalGraph::bind_records`] writes
    /// when the graph skipped the parse entries (a provider called
    /// directly). Both come from the site, so they agree.
    pub fn endpoint_in(&self, nodes: &[RawNode]) -> Option<RawEndpoint> {
        self.endpoint.clone().or_else(|| self.bind(nodes))
    }
}

/// Read vs write classification of a SQL statement, derived from its leading
/// verb. Carried into `Edge.reason` so `ecp impact` can distinguish readers
/// from writers of a table.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum SqlVerb {
    Read,
    Write,
}

impl SqlVerb {
    pub fn as_reason(self) -> &'static str {
        match self {
            SqlVerb::Read => "read",
            SqlVerb::Write => "write",
        }
    }
}

/// A raw-SQL string literal in application code, resolved to the tables it
/// references. Emitted by per-language parsers (same `string_literal` capture
/// hook that feeds `path_literals`) and promoted to `QueriesTable` edges by
/// `post_process::sql_table_edges`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawSqlRef {
    /// Tables referenced by the statement, each with its access verb. Empty
    /// when `unresolved`.
    pub tables: Vec<(String, SqlVerb)>,
    /// True when the SQL could not be statically resolved (interpolated table
    /// name, parse failure) — becomes a `BlindSpot`, never a fabricated edge.
    pub unresolved: bool,
    /// Span of the literal in source.
    pub span: (u32, u32, u32, u32),
    /// Enclosing Function/Method name; `None` for module-top-level literals.
    pub enclosing_symbol: Option<String>,
    /// Owner class when the enclosing symbol is a method.
    pub enclosing_owner: Option<String>,
    /// The caller the statement attaches to: the innermost enclosing callable
    /// or `Const`, else the file (C4 `RecordKind::SqlRef`). Written by
    /// [`LocalGraph::bind_records`]; `None` before binding.
    pub endpoint: Option<RawEndpoint>,
}

impl RawSqlRef {
    pub fn bind(&self, nodes: &[RawNode]) -> Option<RawEndpoint> {
        RecordKind::SqlRef.bind_site(nodes, self.span)
    }

    /// As [`RawPathLiteral::endpoint_in`].
    pub fn endpoint_in(&self, nodes: &[RawNode]) -> Option<RawEndpoint> {
        self.endpoint.clone().or_else(|| self.bind(nodes))
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawDocumentBlock {
    pub name: String,
    pub is_section: bool,
    pub span: (u32, u32, u32, u32),
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawFrameworkRef {
    pub source_name: String,
    pub target_name: String,
    pub confidence: f32,
    pub reason: String,
    pub span: (u32, u32, u32, u32),
}

/// Primitive type of a schema column or model field.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[rkyv(derive(Debug))]
pub enum SchemaType {
    String,
    Int,
    Float,
    Bool,
    Datetime,
    Json,
    Other,
}

/// Message-bus direction for a call site.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum PubSub {
    Publish,
    Subscribe,
}

/// Origin framework that triggered detection of a `SchemaField` /
/// `EventTopic` / `TxScope`. Closed set, stored as u8 so per-instance cost
/// is 1 byte versus ~24 bytes + heap copy for a `String`. Variants are
/// archive-stable as long as new ones are appended (never reordered);
/// `as_str()` round-trips via `FRAMEWORK_NAMES`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum FrameworkId {
    // ── Schema field origins (T4 family) ──
    Pydantic,
    SqlAlchemy,
    Django,
    Prisma,
    Sqlx,
    TypeScriptInterface,
    // ── Event topic transports (T5 family) ──
    Kafka,
    Sns,
    Sqs,
    RabbitMq,
    EventBridge,
    // ── Transaction scopes (T10 family — values observed in PR #272) ──
    SpringTransactional,
    JpaTransactional,
    DjangoAtomic,
    PonyDbSession,
    // ── Schema field origins (T4-5) ──
    Protobuf,
    // ── Event topic transports (T5 family — continued) ──
    /// Celery distributed task queue — task enqueue model (T5-20).
    /// Distinguished from Kafka (durable log) and Redis pub/sub
    /// (fire-and-forget) so LLMs know task invocations are durable
    /// (broker-backed queue with retry semantics).
    Celery,
    /// Redis pub/sub — fire-and-forget channel model (T5-26/T5-27/T5-28).
    /// Distinguished from Kafka (durable log) and RabbitMQ (queued AMQP)
    /// so LLMs know subscribers MUST be online when publish fires.
    Redis,
    // ── Schema field origins (T4-6) ──
    OpenApi,
    Swagger,
    // ── Fallback for frameworks not yet listed; promote to its own variant
    //    when adding emit support, do not extend silently. ──
    Unknown,
    // ── Transaction scopes (T10 family — continued) ──
    /// .NET / EF Core `[Transactional]` or `[TransactionAttribute]`.
    /// Distinct from `SpringTransactional`: .NET uses attribute_list syntax
    /// `[Attr]` vs Java/Kotlin `@Attr`; LLM refactor queries must not
    /// conflate Spring propagation semantics with ADO.NET transaction scope.
    DotNetTransactional,
    /// Symfony PHP 8+ `#[Transactional]` attribute.
    /// Distinct from Spring: PHP 8 attributes use `#[Attr]` syntax; Symfony
    /// transaction semantics (Doctrine ORM session) differ from Spring JPA.
    SymfonyTransactional,
    // ── Transaction scopes (T10 family — FU-2026-05-23-009 cross-lang expansion) ──
    //
    // The next 6 variants pre-allocate rkyv discriminant slots for
    // languages whose parser support is added in sibling commits. Slots
    // are reserved here in ONE place so parallel parser commits don't
    // race on discriminant positions (rkyv discriminants are positional
    // and append-only — reordering would break every existing graph.bin).
    //
    /// TypeScript / NestJS TypeORM `@Transactional` decorator.
    /// Distinct from Spring/Symfony: TS attribute syntax is `@Attr` (same
    /// as Spring) but the runtime is TypeORM / typeorm-transactional —
    /// nested-tx semantics + propagation flags differ from JPA.
    TypeOrmTransactional,
    /// Rust `#[transaction]` proc-macro attribute (sqlx, diesel, sea-orm
    /// flavours). Distinct from Symfony's PHP `#[Attr]` — Rust attributes
    /// are compile-time macros expanding to RAII guards, whereas PHP 8
    /// attributes are runtime-reflected. LLM refactor queries must not
    /// conflate the two.
    RustTransaction,
    /// Dart Drift `transaction(() async { ... })` call-site form, OR
    /// any `@DriftTransaction` annotation if a framework offers one.
    /// Block-form transaction scope — the scope is the closure body, not
    /// an annotated function. Distinct from annotation-form variants
    /// because containment is span-based, not decorator-based.
    DartTransaction,
    /// Go `db.Begin()` / `db.BeginTx(ctx, opts)` call-site. Scope = the
    /// enclosing function. Different detector pattern from annotation-form
    /// (no decorator on the function); parser walks the call AST and
    /// emits the enclosing function as the tx-scope owner via span
    /// containment. Most-used Go idiom — covers `database/sql` and
    /// `gorm.Begin()` variants.
    GoSqlTx,
    /// Ruby `Model.transaction do ... end` block-form (ActiveRecord /
    /// Sequel idiom). Scope = the block body, recovered via span
    /// containment of the enclosing function. FU-2026-05-23-018 had
    /// carved this out as separate scope; consolidated here under
    /// FU-009's full cross-lang expansion per session directive.
    RubyActiveRecordTransaction,
    /// Swift framework-level transactional patterns — most Swift code
    /// uses Core Data `performAndWait` blocks or GRDB `dbQueue.write
    /// { ... }`. Slot reserved; parser may emit zero matches if no
    /// supported framework is detected (parsers wontfix on lang audit
    /// are expected — see PR description for which langs landed real
    /// detectors).
    SwiftTransactional,
}

pub const FRAMEWORK_NAMES: &[&str] = &[
    "pydantic",
    "sqlalchemy",
    "django",
    "prisma",
    "sqlx",
    "typescript-interface",
    "kafka",
    "sns",
    "sqs",
    "rabbitmq",
    "eventbridge",
    "spring-transactional",
    "jpa-transactional",
    "django-atomic",
    "pony-db-session",
    "protobuf",
    "celery",
    "redis",
    "openapi",
    "swagger",
    "unknown",
    "dotnet-transactional",
    "symfony-transactional",
    // FU-2026-05-23-009 cross-lang expansion — order MUST match the
    // FrameworkId enum discriminants above.
    "typeorm-transactional",
    "rust-transaction",
    "dart-transaction",
    "go-sql-tx",
    "ruby-activerecord-transaction",
    "swift-transactional",
];

impl FrameworkId {
    /// Layout-locked with the enum discriminant. Asserted at startup by
    /// `debug_assert_eq!` in tests so a future variant reorder is caught.
    pub const fn as_str(self) -> &'static str {
        FRAMEWORK_NAMES[self as usize]
    }

    /// Decode a u8 (e.g. from a packed bitfield or corrupted archive) into a
    /// FrameworkId. Out-of-range bytes fall back to `Unknown` — preserves
    /// archive read safety when `RawTxScope.packed` is consumed from a
    /// `graph.bin` written by a future version with extra variants.
    ///
    /// Exhaustive `match` instead of bounds-checked `transmute`: adding a new
    /// variant without updating this arm is a compile error, so the
    /// "discriminant ↔ variant" link is enforced by the compiler rather than
    /// by a documented invariant. Modern rustc lowers a 0..N integer match on
    /// a `#[repr(u8)]` enum to a jump table — zero runtime cost vs the
    /// transmute path.
    #[inline]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Pydantic,
            1 => Self::SqlAlchemy,
            2 => Self::Django,
            3 => Self::Prisma,
            4 => Self::Sqlx,
            5 => Self::TypeScriptInterface,
            6 => Self::Kafka,
            7 => Self::Sns,
            8 => Self::Sqs,
            9 => Self::RabbitMq,
            10 => Self::EventBridge,
            11 => Self::SpringTransactional,
            12 => Self::JpaTransactional,
            13 => Self::DjangoAtomic,
            14 => Self::PonyDbSession,
            15 => Self::Protobuf,
            16 => Self::Celery,
            17 => Self::Redis,
            18 => Self::OpenApi,
            19 => Self::Swagger,
            20 => Self::Unknown,
            21 => Self::DotNetTransactional,
            22 => Self::SymfonyTransactional,
            23 => Self::TypeOrmTransactional,
            24 => Self::RustTransaction,
            25 => Self::DartTransaction,
            26 => Self::GoSqlTx,
            27 => Self::RubyActiveRecordTransaction,
            28 => Self::SwiftTransactional,
            _ => Self::Unknown,
        }
    }
}

// Compile-time guard: FRAMEWORK_NAMES MUST have exactly as many entries as
// there are FrameworkId variants (highest discriminant + 1). Catches
// "added an enum variant, forgot the FRAMEWORK_NAMES string" at compile
// time instead of as a runtime panic in `as_str()`.
const _: () = {
    assert!(
        FRAMEWORK_NAMES.len() == FrameworkId::SwiftTransactional as usize + 1,
        "FRAMEWORK_NAMES length must equal FrameworkId variant count"
    );
};

/// ORM / schema model field detected at static-analysis time.
///
/// Field-name + owner-class are stored as owned `Box<str>` rather than
/// `StrRef` because per-language parsers run in isolated scopes — the
/// `StringPool` they intern into is dropped before the `LocalGraph` reaches
/// the builder. Owned strings cost an extra 16 B per field but eliminate
/// the dangling-StrRef pre-T4-7 bug entirely. Aligns with `RawNode.name`
/// which is also `String` for the same reason.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawSchemaField {
    pub name: Box<str>,
    pub type_class: SchemaType,
    pub owner_class: Box<str>,
    pub framework: FrameworkId,
    pub span: (u32, u32, u32, u32),
    /// The owning type (C4 `RecordKind::SchemaField`, no file fallback).
    /// Written by [`LocalGraph::bind_records`]; `None` before binding and
    /// when no type of this file owns the field.
    pub endpoint: Option<RawEndpoint>,
}

impl RawSchemaField {
    /// The innermost enclosing type, when its name is the `owner_class` the
    /// producer read. A producer whose spans do not nest (an OpenAPI schema
    /// whose key spans fell back to `(0, 0, 0, 0)`) binds by that name
    /// instead, and only to a type the site provably sees: two visible
    /// same-name types bind neither.
    pub fn bind(&self, nodes: &[RawNode]) -> Option<RawEndpoint> {
        let owner = &*self.owner_class;
        let by_site = RecordKind::SchemaField.bind_site(nodes, self.span);
        if let Some(RawEndpoint::Node(idx)) = &by_site {
            if nodes[idx.index()].name == owner {
                return by_site;
            }
        }
        let EndpointRule::Enclosing { accept, .. } = RecordKind::SchemaField.endpoint_rule() else {
            return None;
        };
        let bound = bind_name(nodes, self.span, owner, accept);
        bound.endpoint().node().map(RawEndpoint::Node)
    }

    /// As [`RawPathLiteral::endpoint_in`].
    pub fn endpoint_in(&self, nodes: &[RawNode]) -> Option<RawEndpoint> {
        self.endpoint.clone().or_else(|| self.bind(nodes))
    }
}

/// Message-bus publish/subscribe call site.
///
/// `topic_literal` and `enclosing_fn` are owned `Box<str>` so the struct is
/// self-contained after the per-file parse scope drops. Previous
/// `StrRef`-based layout required callers to carry a dropped local pool,
/// making post-process promotion (T5-33) impossible without leaking the
/// pool into `LocalGraph`. `Box<str>` matches `RawSchemaField`'s pattern.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawEventTopic {
    /// The topic as the source spells it, string delimiters stripped.
    /// `None` when the topic string is dynamic (variable arg); emitter
    /// records a `BlindSpot` instead.
    pub topic_literal: Option<Box<str>>,
    /// The pairing key: `topic_literal` run through
    /// `event_topic::normalize::canonicalize` when the detector config asks
    /// for it, else equal to `topic_literal`. `None` exactly when
    /// `topic_literal` is `None`.
    pub topic_key: Option<Box<str>>,
    pub direction: PubSub,
    pub lib: FrameworkId,
    /// Name of the enclosing function/method at the call site.
    /// Empty string when the detector's producer-capture is absent.
    pub enclosing_fn: Box<str>,
    pub span: (u32, u32, u32, u32),
    /// The publisher or subscriber: the innermost enclosing callable, else
    /// the file (C4 `RecordKind::EventTopic`). Written by
    /// [`LocalGraph::bind_records`]; `None` before binding.
    pub endpoint: Option<RawEndpoint>,
}

impl RawEventTopic {
    pub fn bind(&self, nodes: &[RawNode]) -> Option<RawEndpoint> {
        RecordKind::EventTopic.bind_site(nodes, self.span)
    }

    /// As [`RawPathLiteral::endpoint_in`].
    pub fn endpoint_in(&self, nodes: &[RawNode]) -> Option<RawEndpoint> {
        self.endpoint.clone().or_else(|| self.bind(nodes))
    }
}

/// Transactional scope boundary (e.g. `@Transactional`, `atomic()`).
///
/// Packed: high 24 bits = node index into `LocalGraph.nodes` (the
/// Method / Function / Constructor whose body the boundary scopes),
/// low 8 bits = `FrameworkId` discriminant. 4 bytes total — 7× denser
/// than the prior (StrRef + FrameworkId + span) shape because the
/// enclosing node already carries `name`, `span`, and `decorators`.
/// Resolve via `nodes[scope.node_idx() as usize]`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
pub struct RawTxScope {
    /// Bit-packed `(node_idx << 8) | framework_id_u8`. Visibility is
    /// `pub(crate)` so out-of-crate consumers must go through the
    /// `node_idx()` / `framework()` accessor methods — keeps the layout
    /// free to change without breaking external callers.
    pub(crate) packed: u32,
}

impl RawTxScope {
    /// Largest node index that fits in the high 24 bits.
    pub const NODE_IDX_MAX: u32 = (1 << 24) - 1;

    /// Construct a packed scope. `debug_assert!` enforces the u24 limit on
    /// `node_idx` — files with more than 16 M symbol nodes are not realistic
    /// for static analysis and would indicate a generated-code fixture
    /// pathology.
    #[inline]
    pub fn new(node_idx: u32, framework: FrameworkId) -> Self {
        debug_assert!(
            node_idx <= Self::NODE_IDX_MAX,
            "RawTxScope::new: node_idx {} exceeds u24 limit",
            node_idx
        );
        Self {
            packed: (node_idx << 8) | (framework as u32),
        }
    }

    #[inline]
    pub const fn node_idx(self) -> u32 {
        self.packed >> 8
    }

    #[inline]
    pub const fn framework(self) -> FrameworkId {
        FrameworkId::from_u8((self.packed & 0xFF) as u8)
    }
}

impl ArchivedRawTxScope {
    #[inline]
    pub fn node_idx(&self) -> u32 {
        self.packed.to_native() >> 8
    }

    #[inline]
    pub fn framework(&self) -> FrameworkId {
        FrameworkId::from_u8((self.packed.to_native() & 0xFF) as u8)
    }
}

/// Reflection-style fan-out reference: a single call site whose target cannot
/// be uniquely picked at static-analysis time, but where the analyzer can
/// enumerate the candidate set. The builder emits one `References` edge per
/// candidate with confidence `base_confidence / sqrt(N)` (floored at 0.1).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawFanoutRef {
    pub source_name: String,
    pub candidates: Vec<String>,
    pub base_confidence: f32,
    pub reason: String,
    pub span: (u32, u32, u32, u32),
}

/// Truly unresolvable code pattern (eval/exec/dynamic-import/cross-object
/// reflection/...). Unlike `RawFanoutRef`, candidates cannot even be
/// enumerated — the analyzer just records "this is a blind spot" so
/// downstream LLM tooling can flag the location for manual inspection.
///
/// Carries `file_path` directly (unlike other Raw* types whose file is
/// implicit in their owning `LocalGraph`) because blind spots are
/// passed through to graph-level metadata where the source file must
/// remain identifiable after the LocalGraph is consumed.
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct BlindSpot {
    pub kind: String,
    #[rkyv(with = rkyv::with::AsString)]
    pub file_path: PathBuf,
    pub span: (u32, u32, u32, u32),
    pub hint: String,
    /// True iff the BlindSpot originates from a file classified as test
    /// scaffolding (`is_test_path(file_path)`). Populated by each
    /// language parser at emission time; passes through to
    /// `graph::BlindSpotRecord.is_test`. Verdict layer uses this to
    /// suppress test-region noise on prod-refactor PRs.
    pub is_test: bool,
}

/// Per-call-site dispatch annotation produced by the per-language indirect-
/// dispatch detector. Identifies which entry in `RawNode.calls` (by caller
/// name + zero-based index) is non-direct so the builder can emit a
/// `graph::CallMeta` entry keyed on the resulting `Edge` index.
///
/// Only non-direct calls get a `RawCallMeta` — direct calls are the default
/// and are not annotated (saves space; sparse population contract matches
/// `ZeroCopyGraph.call_metas`).
#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct RawCallMeta {
    /// Name of the enclosing `RawNode` (caller) that owns the call.
    pub caller_name: String,
    /// Span of the enclosing caller node. Combined with `call_index` to
    /// uniquely identify same-name functions/methods in one file.
    pub caller_span: (u32, u32, u32, u32),
    /// Zero-based index into `RawNode.calls` for that caller.
    pub call_index: u32,
    /// Packed flags — same bit layout as `graph::CallMeta::flags`.
    /// Populated by the per-language detector; `FLAG_DIRECT` is always clear
    /// (this struct only exists for non-direct calls).
    pub flags: u8,
    /// Dispatch type string as it appears in source (e.g. `"dyn Handler"`,
    /// `"void(*)(int)"`, `"Box<dyn Trait>"`). Empty string when not available.
    pub dispatch_type: String,
}

/// One name a TS / JS module exports and the local declaration it binds:
/// `export { g as f }` is `f` → `g`, `export default h` and
/// `export default function h() {}` are `default` → `h`, and `export function
/// k() {}` is `k` → `k`. `export default wrap(h)` binds no declaration: it is
/// `default` with an empty local.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct RawExportAlias {
    pub exported: Box<str>,
    pub local: Box<str>,
}

/// The variable a route call is made on: `router` in `router.get("/x", h)`,
/// `app` in `app.use("/api", router)`, at the span of that call. The builder
/// reads it to compose router mounts onto the routes of the mounted router.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct RawRouteReceiver {
    pub span: (u32, u32, u32, u32),
    pub receiver: String,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[rkyv(derive(Debug))]
pub struct LocalGraph {
    #[rkyv(with = rkyv::with::AsString)]
    pub file_path: PathBuf,
    pub content_hash: [u8; 8],
    pub nodes: Vec<RawNode>,
    pub documents: Vec<RawDocumentBlock>,
    pub imports: Vec<RawImport>,
    pub routes: Vec<RawRoute>,
    /// The variable each route call is made on, for the parsers that read
    /// router mounts (Express). Kept apart from `routes`: it is no endpoint.
    /// `None` for the files with no such call.
    pub route_receivers: Option<Box<[RawRouteReceiver]>>,
    pub framework_refs: Vec<RawFrameworkRef>,
    pub fanout_refs: Vec<RawFanoutRef>,
    pub blind_spots: Vec<BlindSpot>,
    /// Side tables for T-phase detectors. `None` until the corresponding
    /// detector populates them — saves 16 bytes/field versus an always-empty
    /// `Vec` and skips an archived length marker in `graph.bin` for files
    /// with no schema / event / transaction surface (i.e. the majority).
    pub schema_fields: Option<Box<[RawSchemaField]>>,
    pub event_topics: Option<Box<[RawEventTopic]>>,
    pub tx_scopes: Option<Box<[RawTxScope]>>,
    /// Path-shaped string literals captured by per-language `path_literals`
    /// extractors. Promoted to `NodeKind::PathLiteral` + `UsesPathLiteral`
    /// edges by `post_process::path_literal_nodes`. `None` until at least one
    /// extractor populates it; most files carry no path literals so this stays
    /// cheap to archive.
    pub path_literals: Option<Box<[RawPathLiteral]>>,
    /// Raw-SQL string references captured by per-language parsers; promoted to
    /// `QueriesTable` edges in post-process. `None` when the file has none.
    pub sql_refs: Option<Box<[RawSqlRef]>>,
    /// Indirect-dispatch annotations for individual call sites. Sparse:
    /// only non-direct calls (fn-pointer / vtable / callback / dyn-trait)
    /// have entries. The builder promotes these to `ZeroCopyGraph.call_metas`
    /// once `Edge` indices are known.
    pub call_metas: Vec<RawCallMeta>,
    /// Per-function metadata captured during parsing. Paired with `nodes` by
    /// span at build time; only populated for `Function`/`Method`/`Constructor`
    /// nodes. Empty for languages not yet covered by Phase 1/2.
    pub raw_function_metas: Vec<RawFunctionMeta>,
    /// TS / JS export names that differ from the declaration they bind, with
    /// every other export of the same file, so an import of a name the file
    /// exports only under another name binds nothing. `None` when every
    /// export keeps its declaration's name (most files) and for other
    /// languages.
    pub export_aliases: Option<Box<[RawExportAlias]>>,
}

impl LocalGraph {
    /// C4 `ParsePhase::BindRecords`: write the endpoint of every path
    /// literal, SQL reference, schema field and event topic from its site,
    /// and the handler of every route ([`RawRoute::bind`]).
    /// Runs once the provider has returned, when the node list is final;
    /// nodes stay untouched.
    pub fn bind_records(&mut self) {
        let nodes = &self.nodes;
        for r in &mut self.routes {
            r.handler = r.bind(nodes);
        }
        for r in self.path_literals.iter_mut().flat_map(|b| b.iter_mut()) {
            r.endpoint = r.bind(nodes);
        }
        for r in self.sql_refs.iter_mut().flat_map(|b| b.iter_mut()) {
            r.endpoint = r.bind(nodes);
        }
        for r in self.schema_fields.iter_mut().flat_map(|b| b.iter_mut()) {
            r.endpoint = r.bind(nodes);
        }
        for r in self.event_topics.iter_mut().flat_map(|b| b.iter_mut()) {
            r.endpoint = r.bind(nodes);
        }
    }

    /// A graph for a file the pipeline could not analyse, carrying nothing but
    /// the reason.
    ///
    /// Every path that gives up on a file used to `return` from the worker, so
    /// the file contributed no graph and no record, and the index published as
    /// complete. `ecp find` then answered `found: false` for a symbol that is
    /// in the tree — and `found:false` with no caveat is the tool telling the
    /// reading model the symbol does not exist. The project's own rule is that
    /// honest no-data beats a guess; this is what makes the no-data honest.
    ///
    /// `content_hash` stays zero on purpose. It is the parse cache's key, and a
    /// hash of content that was never read would be a lie the next run trusts.
    pub fn omitted(rel_path: &std::path::Path, kind: &str, hint: String) -> Self {
        let is_test = crate::file_category::is_test_path(&rel_path.to_string_lossy());
        Self {
            file_path: rel_path.to_path_buf(),
            content_hash: [0u8; 8],
            nodes: Vec::new(),
            documents: Vec::new(),
            imports: Vec::new(),
            routes: Vec::new(),
            route_receivers: None,
            framework_refs: Vec::new(),
            fanout_refs: Vec::new(),
            blind_spots: vec![BlindSpot {
                kind: kind.to_string(),
                file_path: rel_path.to_path_buf(),
                span: (0, 0, 0, 0),
                hint,
                is_test,
            }],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: Vec::new(),
            raw_function_metas: Vec::new(),
            export_aliases: None,
        }
    }
}

impl Default for LocalGraph {
    fn default() -> Self {
        Self {
            file_path: PathBuf::new(),
            content_hash: [0; 8],
            nodes: Vec::new(),
            documents: Vec::new(),
            imports: Vec::new(),
            routes: Vec::new(),
            route_receivers: None,
            framework_refs: Vec::new(),
            fanout_refs: Vec::new(),
            blind_spots: Vec::new(),
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: Vec::new(),
            raw_function_metas: Vec::new(),
            export_aliases: None,
        }
    }
}

/// One enum-variant reference a parser recorded in [`RawNode::field_reads`]:
/// `E::A { .. }`, `E::B(..)`, `Day.MONDAY`, a `case Color::Red:` label, or a
/// bare enumerator (`RED`) where the language names variants unqualified.
///
/// The entry rides in `field_reads` behind a U+0001 marker, like the
/// [`CallSite`] markers in `calls`: no field name holds a control
/// character, so the `ReadsField` tier never sees one. Pass 2 resolves the
/// path's qualifier as a type and the last segment as one of that type's
/// variants (`References` edge); a bare name resolves only to a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantRef<'a> {
    /// `Q::V`, `pkg.Q.V`, `\App\Q::V`, or a bare `V`, whitespace removed.
    pub path: &'a str,
    /// The reference sits in a pattern or case label, so it tests for the
    /// variant rather than producing or reading it.
    pub pattern: bool,
}

impl<'a> VariantRef<'a> {
    const USE_PREFIX: &'static str = "\u{1}variant ";
    const PATTERN_PREFIX: &'static str = "\u{1}variant-pattern ";

    pub fn encode(path: &str, pattern: bool) -> String {
        let prefix = if pattern {
            Self::PATTERN_PREFIX
        } else {
            Self::USE_PREFIX
        };
        [prefix, path].concat()
    }

    /// `None` for an ordinary field-read entry.
    pub fn parse(raw: &'a str) -> Option<Self> {
        if let Some(path) = raw.strip_prefix(Self::PATTERN_PREFIX) {
            Some(Self {
                path,
                pattern: true,
            })
        } else {
            raw.strip_prefix(Self::USE_PREFIX).map(|path| Self {
                path,
                pattern: false,
            })
        }
    }

    /// `(qualifier path, variant)` split at the last `::` / `.` / `\`;
    /// `None` for a bare name.
    pub fn split(self) -> Option<(&'a str, &'a str)> {
        let path = self.path;
        let idx = [
            path.rfind("::").map(|i| (i, 2)),
            path.rfind('.').map(|i| (i, 1)),
        ]
        .into_iter()
        .flatten()
        .max_by_key(|&(i, _)| i)?;
        let (qualifier, variant) = (&path[..idx.0], &path[idx.0 + idx.1..]);
        (!qualifier.is_empty() && !variant.is_empty()).then_some((qualifier, variant))
    }
}

/// The `References` reason of a variant used as a value: constructed, read,
/// compared.
pub const VARIANT_USE_REASON: &str = "enum_variant:use";
/// The `References` reason of a variant in a pattern or case label.
pub const VARIANT_PATTERN_REASON: &str = "enum_variant:pattern";

/// What the bare name of a [`NameRef`] may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameRefKind {
    /// A module-level / static constant or variable it reads, or a function
    /// it uses as a value: passed as a callback, stored, returned.
    Value,
    /// A constant only: a PHP / Ruby bare name never names a function.
    Constant,
    /// A function only: a method reference (`Foo::bar`, `::f`, Ruby
    /// `method(:f)`). A qualified name is written `Foo.bar`.
    Function,
}

/// One name a function body uses as a value, not as a callee: `LIMITS` in
/// `LIMITS.max`, `handler` in `addEventListener('x', handler)`. Parameters
/// and locals of the enclosing function are never recorded.
///
/// The entry rides in [`RawNode::field_reads`] behind a U+0001 marker, like
/// [`VariantRef`]. Pass 2 binds a constant or variable as a `ReadsField`
/// edge ([`VALUE_READ_REASON`]) and a function as a `References` edge
/// ([`FUNCTION_REFERENCE_REASON`]), never as a `Calls` edge: the function
/// is registered or handed on here, not invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameRef<'a> {
    pub name: &'a str,
    pub kind: NameRefKind,
}

impl<'a> NameRef<'a> {
    const VALUE_PREFIX: &'static str = "\u{1}name ";
    const CONSTANT_PREFIX: &'static str = "\u{1}const ";
    const FUNCTION_PREFIX: &'static str = "\u{1}fnref ";

    const fn prefix(kind: NameRefKind) -> &'static str {
        match kind {
            NameRefKind::Value => Self::VALUE_PREFIX,
            NameRefKind::Constant => Self::CONSTANT_PREFIX,
            NameRefKind::Function => Self::FUNCTION_PREFIX,
        }
    }

    pub fn encode(name: &str, kind: NameRefKind) -> String {
        [Self::prefix(kind), name].concat()
    }

    /// `None` for an ordinary field-read or variant entry.
    pub fn parse(raw: &'a str) -> Option<Self> {
        [
            NameRefKind::Value,
            NameRefKind::Constant,
            NameRefKind::Function,
        ]
        .into_iter()
        .find_map(|kind| {
            raw.strip_prefix(Self::prefix(kind))
                .map(|name| Self { name, kind })
        })
    }
}

/// The `ReadsField` reason of a constant or variable a function names bare,
/// apart from `field_read` (`obj.field`).
pub const VALUE_READ_REASON: &str = "value_read";
/// The `References` reason of a function named as a value (callback
/// argument, stored handler, method reference) rather than called.
pub const FUNCTION_REFERENCE_REASON: &str = "function_reference";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_sql_ref_carries_tables_and_verb() {
        let r = RawSqlRef {
            tables: vec![("channels".to_string(), SqlVerb::Read)],
            unresolved: false,
            span: (1, 0, 1, 40),
            enclosing_symbol: Some("list_channels".to_string()),
            enclosing_owner: None,
            endpoint: None,
        };
        assert_eq!(r.tables[0].0, "channels");
        assert_eq!(r.tables[0].1, SqlVerb::Read);
        assert!(!r.unresolved);
        assert_eq!(SqlVerb::Read.as_reason(), "read");
        assert_eq!(SqlVerb::Write.as_reason(), "write");
    }

    use crate::contracts::raw_records::RawIdx;

    fn bind_node(name: &str, kind: NodeKind, span: Span) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind,
            span,
            is_exported: false,
            heritage: Vec::new(),
            type_annotation: None,
            decorators: Vec::new(),
            calls: Vec::new(),
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
        }
    }

    fn schema_field(owner: &str, span: Span) -> RawSchemaField {
        RawSchemaField {
            name: "email".into(),
            type_class: SchemaType::String,
            owner_class: owner.into(),
            framework: FrameworkId::Pydantic,
            span,
            endpoint: None,
        }
    }

    fn event_topic(span: Span) -> RawEventTopic {
        RawEventTopic {
            topic_literal: Some("orders".into()),
            topic_key: Some("orders".into()),
            direction: PubSub::Publish,
            lib: FrameworkId::Pydantic,
            enclosing_fn: "".into(),
            span,
            endpoint: None,
        }
    }

    #[test]
    fn test_bind_schema_field_inside_owner_type_binds_that_type() {
        let nodes = [
            bind_node("User", NodeKind::Class, (0, 0, 3, 0)),
            bind_node("Order", NodeKind::Class, (4, 0, 7, 0)),
        ];
        assert_eq!(
            schema_field("Order", (5, 4, 5, 20)).bind(&nodes),
            Some(RawEndpoint::Node(RawIdx(1)))
        );
    }

    /// An OpenAPI schema whose key spans fell back to `(0, 0, 0, 0)`: every
    /// type contains every field by span, so the owner's name decides.
    #[test]
    fn test_bind_schema_field_zero_spans_binds_by_owner_name() {
        let zero = (0, 0, 0, 0);
        let nodes = [
            bind_node("User", NodeKind::Struct, zero),
            bind_node("Order", NodeKind::Struct, zero),
        ];
        assert_eq!(
            schema_field("Order", zero).bind(&nodes),
            Some(RawEndpoint::Node(RawIdx(1)))
        );
    }

    #[test]
    fn test_bind_schema_field_two_visible_same_name_types_binds_none() {
        let zero = (0, 0, 0, 0);
        let nodes = [
            bind_node("Other", NodeKind::Struct, zero),
            bind_node("Order", NodeKind::Struct, (1, 0, 2, 0)),
            bind_node("Order", NodeKind::Struct, (3, 0, 4, 0)),
        ];
        assert_eq!(schema_field("Order", zero).bind(&nodes), None);
    }

    #[test]
    fn test_bind_schema_field_no_type_or_blank_owner_binds_none() {
        let nodes = [bind_node("load", NodeKind::Function, (0, 0, 3, 0))];
        assert_eq!(schema_field("User", (1, 0, 1, 9)).bind(&nodes), None);
        assert_eq!(schema_field("  ", (1, 0, 1, 9)).bind(&nodes), None);
        assert_eq!(schema_field("User", (1, 0, 1, 9)).bind(&[]), None);
    }

    #[test]
    fn test_bind_event_topic_module_top_binds_file_inside_callable_binds_it() {
        let nodes = [bind_node("publish", NodeKind::Function, (2, 0, 4, 0))];
        assert_eq!(
            event_topic((0, 0, 0, 30)).bind(&nodes),
            Some(RawEndpoint::File)
        );
        assert_eq!(
            event_topic((3, 4, 3, 30)).bind(&nodes),
            Some(RawEndpoint::Node(RawIdx(0)))
        );
    }

    /// Binding twice (a cached graph re-entering a parse entry) writes the
    /// same endpoints; a graph with no records stays untouched.
    #[test]
    fn test_bind_records_twice_writes_same_endpoints_and_empty_graph_is_noop() {
        let mut graph = LocalGraph {
            nodes: vec![
                bind_node("load", NodeKind::Function, (2, 0, 4, 0)),
                bind_node("User", NodeKind::Class, (6, 0, 8, 0)),
            ],
            path_literals: Some(Box::new([RawPathLiteral {
                value: "conf/a.json".into(),
                span: (3, 4, 3, 20),
                enclosing_symbol: Some("load".into()),
                enclosing_owner: None,
                sink_reason: String::new(),
                endpoint: None,
            }])),
            sql_refs: Some(Box::new([RawSqlRef {
                tables: Vec::new(),
                unresolved: false,
                span: (0, 0, 0, 30),
                enclosing_symbol: None,
                enclosing_owner: None,
                endpoint: None,
            }])),
            schema_fields: Some(Box::new([schema_field("User", (7, 4, 7, 20))])),
            event_topics: Some(Box::new([event_topic((9, 0, 9, 30))])),
            ..LocalGraph::default()
        };
        graph.bind_records();
        let endpoints = |g: &LocalGraph| {
            (
                g.path_literals.as_ref().unwrap()[0].endpoint.clone(),
                g.sql_refs.as_ref().unwrap()[0].endpoint.clone(),
                g.schema_fields.as_ref().unwrap()[0].endpoint.clone(),
                g.event_topics.as_ref().unwrap()[0].endpoint.clone(),
            )
        };
        let first = endpoints(&graph);
        assert_eq!(
            first,
            (
                Some(RawEndpoint::Node(RawIdx(0))),
                Some(RawEndpoint::File),
                Some(RawEndpoint::Node(RawIdx(1))),
                Some(RawEndpoint::File),
            )
        );
        graph.bind_records();
        assert_eq!(endpoints(&graph), first);

        let mut empty = LocalGraph::default();
        empty.bind_records();
        assert!(empty.path_literals.is_none() && empty.schema_fields.is_none());
    }

    fn route(handler: Option<RawEndpoint>, span: Span) -> RawRoute {
        RawRoute {
            method: "GET".into(),
            path: "/x".into(),
            handler,
            span,
            scope: None,
        }
    }

    fn named(name: &str) -> Option<RawEndpoint> {
        RawRoute::named_handler(Some(name))
    }

    fn inline(at: Span) -> Option<RawEndpoint> {
        Some(RawEndpoint::Unresolved(Unbound::Inline(at)))
    }

    /// `app.get('/x', list)` inside `setup`: the file declares a class
    /// member `list` before the top-level `list`. JS binds the bare name to
    /// the top-level declaration, never to the member a name lookup finds
    /// first.
    #[test]
    fn test_raw_route_bind_class_member_before_top_level_binds_top_level() {
        let nodes = [
            bind_node("Admin", NodeKind::Class, (0, 0, 4, 1)),
            bind_node("list", NodeKind::Method, (1, 2, 3, 3)),
            bind_node("list", NodeKind::Function, (6, 0, 8, 1)),
            bind_node("setup", NodeKind::Function, (10, 0, 12, 1)),
        ];
        assert_eq!(
            route(named("list"), (11, 2, 11, 30)).bind(&nodes),
            Some(RawEndpoint::Node(RawIdx(2)))
        );
    }

    /// `@Get('/x')` in a class body decorates the method that follows it,
    /// although a top-level `find` is visible from the decorator.
    #[test]
    fn test_raw_route_bind_decorator_site_binds_decorated_member() {
        let nodes = [
            bind_node("Users", NodeKind::Class, (0, 0, 6, 1)),
            bind_node("find", NodeKind::Method, (3, 2, 5, 3)),
            bind_node("find", NodeKind::Function, (8, 0, 9, 1)),
        ];
        assert_eq!(
            route(named("find"), (2, 3, 2, 6)).bind(&nodes),
            Some(RawEndpoint::Node(RawIdx(1)))
        );
    }

    /// A decorator whose next declaration has another name binds by
    /// visibility; two visible top-level `h` (a redefinition) bind neither,
    /// and the name stays for the unresolved report.
    #[test]
    fn test_raw_route_bind_two_visible_same_name_stays_named() {
        let nodes = [
            bind_node("h", NodeKind::Function, (0, 0, 1, 0)),
            bind_node("h", NodeKind::Function, (2, 0, 3, 0)),
            bind_node("setup", NodeKind::Function, (5, 0, 7, 0)),
        ];
        let r = route(named("h"), (6, 2, 6, 20));
        assert_eq!(r.bind(&nodes), named("h"));
        assert_eq!(r.handler_name(), Some("h"));
        assert_eq!(
            route(named("gone"), (4, 0, 4, 9)).bind(&nodes),
            named("gone")
        );
    }

    /// An inline handler binds the closure node its own argument became,
    /// never a middleware closure of the same registration, nor a closure
    /// nested in its body. Without a node of its own it stays inline.
    /// On HEAD the rule took the one outermost closure anywhere in the
    /// registration, so the middleware at 3:18 served the route.
    #[test]
    fn test_raw_route_bind_inline_binds_handler_argument_closure_only() {
        let nodes = [
            bind_node("main", NodeKind::Function, (0, 0, 20, 1)),
            // `app.get("/x", (req, res, next) => next(), (req, res) => {})`:
            // the middleware has a node, the empty handler has none.
            bind_node("<anonymous:3:18>", NodeKind::Function, (2, 18, 2, 40)),
            // `app.get("/y", (req, res) => { items.forEach(i => use(i)) })`:
            // the handler and the closure in its body both have nodes.
            bind_node("<anonymous:6:16>", NodeKind::Function, (5, 16, 7, 3)),
            bind_node("<anonymous:7:20>", NodeKind::Function, (6, 20, 6, 38)),
            // A named function at the handler's position is no closure.
            bind_node("named", NodeKind::Function, (9, 16, 9, 30)),
        ];
        let empty_handler = (2, 42, 2, 60);
        assert_eq!(
            route(inline(empty_handler), (2, 2, 2, 61)).bind(&nodes),
            inline(empty_handler)
        );
        assert_eq!(
            route(inline((2, 18, 2, 40)), (2, 2, 2, 41)).bind(&nodes),
            Some(RawEndpoint::Node(RawIdx(1)))
        );
        assert_eq!(
            route(inline((5, 16, 7, 3)), (5, 2, 7, 4)).bind(&nodes),
            Some(RawEndpoint::Node(RawIdx(2)))
        );
        assert_eq!(
            route(inline((9, 16, 9, 30)), (9, 2, 9, 31)).bind(&nodes),
            inline((9, 16, 9, 30))
        );
    }

    /// An `Anonymous` handler has no argument span, so it never binds.
    #[test]
    fn test_raw_route_bind_anonymous_without_span_unchanged() {
        let nodes = [bind_node(
            "<anonymous:1:0>",
            NodeKind::Function,
            (0, 0, 2, 1),
        )];
        let anonymous = Some(RawEndpoint::Unresolved(Unbound::Anonymous));
        assert_eq!(
            route(anonymous.clone(), (0, 0, 2, 2)).bind(&nodes),
            anonymous
        );
    }

    /// A Laravel action, an include record's controller, an absent handler
    /// and a blank name never bind in the file.
    #[test]
    fn test_raw_route_bind_scoped_include_absent_and_blank_unchanged() {
        let nodes = [bind_node("index", NodeKind::Function, (0, 0, 2, 0))];
        let mut scoped = route(named("index"), (4, 0, 4, 30));
        scoped.scope = Some(Box::default());
        assert_eq!(scoped.bind(&nodes), named("index"));
        let mut include = route(named("index"), (4, 0, 4, 30));
        include.method = RawRoute::INCLUDE.into();
        assert_eq!(include.bind(&nodes), named("index"));
        assert_eq!(route(None, (4, 0, 4, 30)).bind(&nodes), None);
        assert_eq!(RawRoute::named_handler(Some(" \t")), None);
        assert_eq!(RawRoute::named_handler(None), None);
    }

    /// `bind_records` writes route handlers too, and a second run keeps them.
    #[test]
    fn test_bind_records_route_handler_twice_same_endpoint() {
        let mut graph = LocalGraph {
            nodes: vec![
                bind_node("list", NodeKind::Function, (0, 0, 2, 0)),
                bind_node("setup", NodeKind::Function, (4, 0, 6, 0)),
            ],
            routes: vec![route(named("list"), (5, 2, 5, 30))],
            ..LocalGraph::default()
        };
        graph.bind_records();
        assert_eq!(graph.routes[0].handler, Some(RawEndpoint::Node(RawIdx(0))));
        graph.bind_records();
        assert_eq!(graph.routes[0].handler, Some(RawEndpoint::Node(RawIdx(0))));
    }

    #[test]
    fn test_owner_key_paths_and_generics_returns_last_bare_segment() {
        for (raw, want) in [
            ("Dog", Some("Dog")),
            ("Foo<T>", Some("Foo")),
            ("ns::Foo", Some("Foo")),
            ("A::B", Some("B")),
            ("Outer.Inner", Some("Inner")),
            ("\\App\\Model", Some("Model")),
            ("Map<K, V>::Entry", Some("Entry")),
            ("Outer<T>.Inner", Some("Inner")),
            ("Foo<a::B>", Some("Foo")),
            ("List[int]", Some("List")),
            ("  Dog ", Some("Dog")),
            ("", None),
            ("   ", None),
            ("Foo::", None),
        ] {
            assert_eq!(owner_key(raw), want, "owner_key({raw:?})");
        }
    }

    /// Contract: an encoded entry decodes to its own kind, and callee text
    /// that merely starts like a marker stays a plain call.
    #[test]
    fn test_call_site_parse_encoded_and_lookalike_text_returns_kind() {
        assert_eq!(
            CallSite::parse(&CallSite::construct("ns.Widget")),
            CallSite::Construct("ns.Widget")
        );
        assert_eq!(
            CallSite::parse(&CallSite::untyped_member("widget")),
            CallSite::UntypedMember("widget")
        );
        for text in ["new Router().Handler", "member widget", "Widget", ""] {
            assert_eq!(CallSite::parse(text), CallSite::Plain(text));
        }
    }

    /// Contract: a chained entry decodes to its receiver call's own entry
    /// and the fallback, and every tier that does not type the receiver
    /// sees exactly the fallback the parser wrote before chains existed.
    #[test]
    fn test_call_site_parse_chained_member_returns_inner_and_fallback() {
        let untyped = CallSite::untyped_member("bar");
        let raw = CallSite::chained_member("Foo.create", &untyped);
        let site = CallSite::parse(&raw);
        assert_eq!(
            site,
            CallSite::ChainedMember {
                inner: "Foo.create",
                fallback: untyped.as_str()
            }
        );
        assert_eq!(site.fallback(), CallSite::UntypedMember("bar"));
        assert_eq!(site.name(), "bar");
        assert!(site.uses_method_syntax());
        assert_eq!(site.constructed_type(Language::Python), None);

        let entry = CallSite::chained_member("make", "bar");
        let bare = CallSite::parse(&entry);
        assert_eq!(bare.fallback(), CallSite::Plain("bar"));
        assert_eq!(bare.name(), "bar");
    }

    /// Contract: a chain on a chain keeps the whole inner entry, so the
    /// resolver can walk `a().b().c()` from the innermost call outwards.
    #[test]
    fn test_call_site_parse_nested_chain_returns_inner_chain() {
        let inner = CallSite::chained_member(&CallSite::construct("Foo"), "b");
        let raw = CallSite::chained_member(&inner, &CallSite::untyped_member("c"));
        let CallSite::ChainedMember {
            inner: got_inner,
            fallback,
        } = CallSite::parse(&raw)
        else {
            panic!("not a chain: {raw:?}");
        };
        assert_eq!(got_inner, inner);
        assert_eq!(CallSite::parse(fallback), CallSite::UntypedMember("c"));
        assert_eq!(
            CallSite::parse(got_inner),
            CallSite::ChainedMember {
                inner: "\u{1}new Foo",
                fallback: "b"
            }
        );
        // Lookalike source text and a marker with no separator stay plain.
        for text in ["chain bar", "\u{1}chain bar"] {
            assert_eq!(CallSite::parse(text), CallSite::Plain(text));
        }
    }

    /// Contract: a chain resolves innermost first; a level the typed step
    /// answers never resolves its fallback, and a level it declines does.
    #[test]
    fn test_call_site_resolve_chain_typed_then_fallback_returns_outermost_targets() {
        let inner = CallSite::chained_member("make", "b");
        let raw = CallSite::chained_member(&inner, &CallSite::untyped_member("c"));
        let mut resolved = Vec::new();
        let got = CallSite::resolve_chain(
            &raw,
            |site, _| {
                resolved.push(site.name().to_string());
                format!("resolved:{}", site.name())
            },
            |below, member, _| (member == "b").then(|| format!("{below}>typed:{member}")),
        );
        assert_eq!(resolved, ["make", "c"]);
        assert_eq!(got, "resolved:c");

        let mut resolved = Vec::new();
        let got = CallSite::resolve_chain(
            &raw,
            |site, _| {
                resolved.push(site.name().to_string());
                format!("resolved:{}", site.name())
            },
            |below, member, _| Some(format!("{below}>typed:{member}")),
        );
        assert_eq!(resolved, ["make"]);
        assert_eq!(got, "resolved:make>typed:b>typed:c");

        let plain = CallSite::resolve_chain("f", |site, _| site.name(), |_, _, _| None);
        assert_eq!(plain, "f");
    }

    #[test]
    fn test_call_site_resolve_chain_cut_root_resolves_outermost_fallback_untyped() {
        let inner = CallSite::chained_member(CallSite::CUT_CHAIN, "b");
        let raw = CallSite::chained_member(&inner, "c");
        let mut resolved = Vec::new();
        let got = CallSite::resolve_chain(
            &raw,
            |site, _| {
                resolved.push(site.name().to_string());
                site.name().to_string()
            },
            |below, member, _| Some(format!("{below}>typed:{member}")),
        );
        assert_eq!(resolved, ["c"]);
        assert_eq!(got, "c");
    }

    /// Encodes a chain `levels` calls deep the way a parser's encoder does.
    fn encode_chain(levels: u32) -> Option<String> {
        if levels == 0 {
            return Some("root".to_string());
        }
        Some(match CallSite::nested_chain(|| encode_chain(levels - 1)) {
            Some(inner) => CallSite::chained_member(&inner, "m"),
            None => "m".to_string(),
        })
    }

    fn chain_depth(raw: &str) -> (u32, CallSite<'_>) {
        let mut site = CallSite::parse(raw);
        let mut depth = 0;
        while let CallSite::ChainedMember { inner, .. } = site {
            depth += 1;
            site = CallSite::parse(inner);
        }
        (depth, site)
    }

    #[test]
    fn test_nested_chain_past_limit_returns_cut_root() {
        let deep = encode_chain(CHAIN_DEPTH_LIMIT + 5).unwrap();
        assert_eq!(
            chain_depth(&deep),
            (CHAIN_DEPTH_LIMIT + 1, CallSite::Plain(CallSite::CUT_CHAIN))
        );
        let shallow = encode_chain(CHAIN_DEPTH_LIMIT).unwrap();
        assert_eq!(
            chain_depth(&shallow),
            (CHAIN_DEPTH_LIMIT, CallSite::Plain("root"))
        );
    }

    #[test]
    fn test_nested_chain_missing_receiver_restores_depth() {
        assert_eq!(
            CallSite::nested_chain(|| CallSite::nested_chain(|| None)),
            None
        );
        assert_eq!(CHAIN_DEPTH.get(), 0);
        let after = encode_chain(CHAIN_DEPTH_LIMIT).unwrap();
        assert_eq!(chain_depth(&after).0, CHAIN_DEPTH_LIMIT);
    }

    #[test]
    fn test_nested_chain_unwinding_encoder_restores_depth() {
        let unwound = std::panic::catch_unwind(|| {
            CallSite::nested_chain(|| CallSite::nested_chain(|| panic!("encoder failed")))
        });
        assert!(unwound.is_err());
        assert_eq!(CHAIN_DEPTH.get(), 0);
        let after = encode_chain(CHAIN_DEPTH_LIMIT).unwrap();
        assert_eq!(chain_depth(&after).0, CHAIN_DEPTH_LIMIT);
    }

    /// Contract: a `from`-receiver entry decodes to its receiver and member,
    /// and keeps the member as the name every callable tier resolves.
    #[test]
    fn test_call_site_parse_from_receiver_returns_receiver_and_member() {
        let raw = CallSite::from_receiver("mod", "compute");
        let site = CallSite::parse(&raw);
        assert_eq!(
            site,
            CallSite::FromReceiver {
                receiver: "mod",
                member: "compute"
            }
        );
        assert_eq!(site.name(), "compute");
        assert_eq!(
            site.constructed_type(Language::Python),
            Some(("compute", false))
        );
        assert_eq!(
            CallSite::parse("from mod.compute"),
            CallSite::Plain("from mod.compute")
        );
        let malformed = "\u{1}from compute";
        assert_eq!(CallSite::parse(malformed), CallSite::Plain(malformed));
    }

    /// Contract: an argument record changes no decoded site. Each entry
    /// decodes with the record as it decodes without it, and an entry with
    /// no record reads no arguments.
    #[test]
    fn test_call_site_with_arguments_decodes_as_bare_entry() {
        let chain = CallSite::chained_member("make", "bar");
        let entries = [
            "Widget.Spin".to_string(),
            "Spin".to_string(),
            CallSite::construct("Widget"),
            CallSite::untyped_member("bar"),
            CallSite::from_receiver("Util", "Make"),
            chain,
            String::new(),
        ];
        for entry in &entries {
            let recorded = CallSite::with_arguments(entry, 2, &["times"]);
            assert_eq!(
                CallSite::parse(&recorded),
                CallSite::parse(entry),
                "{entry:?}"
            );
            assert_eq!(CallSite::arguments(entry), None, "{entry:?}");
            let arguments = CallSite::arguments(&recorded).expect("recorded");
            assert_eq!(arguments.positional, 2);
            assert_eq!(arguments.named().collect::<Vec<_>>(), ["times"]);
        }
        let none = CallSite::with_arguments("Widget.Spin", 0, &[]);
        let arguments = CallSite::arguments(&none).expect("recorded");
        assert_eq!(arguments.positional, 0);
        assert_eq!(arguments.named().count(), 0);
        // Lookalike text and a record without a separator stay plain.
        for text in ["args 1", "\u{1}args 1"] {
            assert_eq!(CallSite::parse(text), CallSite::Plain(text));
            assert_eq!(CallSite::arguments(text), None);
        }
    }

    /// Contract: each chain level reads its own record. The innermost site
    /// gets the inner call's arguments, and the typed level and its
    /// fallback get the outer call's.
    #[test]
    fn test_call_site_resolve_chain_hands_each_level_its_arguments() {
        let inner = CallSite::with_arguments("Widget.Make", 1, &[]);
        let raw = CallSite::with_arguments(&CallSite::chained_member(&inner, "Spin"), 0, &["n"]);
        let seen: std::cell::RefCell<Vec<(String, Option<u32>)>> = Default::default();
        CallSite::resolve_chain(
            &raw,
            |site, arguments| {
                seen.borrow_mut()
                    .push((site.name().to_string(), arguments.map(|a| a.positional)));
            },
            |_, member, arguments| {
                let named: Vec<&str> = arguments.into_iter().flat_map(|a| a.named()).collect();
                seen.borrow_mut().push((
                    format!("typed {member} {named:?}"),
                    arguments.map(|a| a.positional),
                ));
                None
            },
        );
        assert_eq!(
            seen.into_inner(),
            [
                ("Widget.Make".to_string(), Some(1)),
                ("typed Spin [\"n\"]".to_string(), Some(0)),
                ("Spin".to_string(), Some(0)),
            ]
        );
    }
}
