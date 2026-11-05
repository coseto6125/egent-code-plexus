//! The receiver-typing ladder: `qualifier.member` resolved through the
//! member's owning type, its declared supertypes included, once the
//! qualifier names project types. Index-time resolution (`Resolver`) and the
//! session overlay find the candidate types each by their own import rules,
//! then read the ownership walk from here through [`TypeFacts`].

use crate::analyzer::reexport::ResolveTarget;
use crate::analyzer::same_file_pick::PickFacts;
use crate::analyzer::types::{owner_key, RawImport, THIS_MEMBER_PREFIX};
use crate::file_category::{FileMeta, Language};
use crate::graph::NodeKind;
use std::borrow::Cow;
use std::path::Path;

/// Depth cap of a supertype walk. Declared heritage deeper than this is
/// rare, and the cap bounds the walk on a corrupt or cyclic chain.
pub const MAX_HERITAGE_DEPTH: usize = 8;

/// The project types a qualifier can name (ladder step 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeCandidates<'q> {
    /// One type, with the name it is declared under (an import alias names
    /// the type differently from the qualifier).
    Unique(u32, &'q str),
    /// Several types, all declared under the one name carried.
    Set(Vec<u32>, &'q str),
    /// An import binds the qualifier to a file outside the project.
    External,
    None,
}

/// The result of resolving a typed chain step.
///
/// `Conclusive` suppresses the parser-name fallback. `Unknown` permits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainOutcome<T> {
    Bound(T),
    Conclusive,
    Unknown,
}

impl<T> ChainOutcome<T> {
    pub fn bound(self) -> Option<T> {
        match self {
            Self::Bound(value) => Some(value),
            Self::Conclusive | Self::Unknown => None,
        }
    }
}

/// Resolve an otherwise-unmodeled return type from project candidates.
///
/// A missing project type is conclusive. Several project types leave the
/// parser fallback available because no type identity is proven.
pub fn unmodeled_return_type<'q>(
    facts: &impl TypeFacts,
    source: &str,
    name: &'q str,
) -> ChainOutcome<TypeCandidates<'q>> {
    let candidates = facts.global_candidates(name, ResolveTarget::Type, facts.file_meta(source));
    let mut candidates = distinct_types(facts, candidates.into_iter());
    match (candidates.next(), candidates.next()) {
        (None, None) => ChainOutcome::Conclusive,
        (Some(id), None) => ChainOutcome::Bound(TypeCandidates::Unique(id, name)),
        (Some(_), Some(_)) => ChainOutcome::Unknown,
        (None, Some(_)) => unreachable!("iterator cannot yield a second item first"),
    }
}

/// `candidates` with the parts of one C# `partial` type read as one type:
/// its representative ([`TypeFacts::partial_type`]), yielded once, at the
/// place of its first part.
pub fn distinct_types<'f, F: TypeFacts + ?Sized>(
    facts: &'f F,
    candidates: impl Iterator<Item = u32> + 'f,
) -> impl Iterator<Item = u32> + 'f {
    let mut seen: Vec<u32> = Vec::new();
    candidates.filter_map(move |id| {
        let ty = facts.partial_type(id);
        (!seen.contains(&ty)).then(|| {
            seen.push(ty);
            ty
        })
    })
}

/// Whether a type owns a member, directly or through its supertypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Owned {
        id: u32,
        inherited: bool,
    },
    NotOwned,
    /// The member may come from a base the index cannot see, or from one of
    /// several owners: no edge.
    Unknown,
}

/// The owner index's answer for one type and one member name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberLookup {
    Hit(u32),
    Miss,
    Ambiguous,
}

/// Outcome of the ladder for one qualified callee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeMember {
    /// The one candidate type declares the member (the qualifier tier's
    /// confidence).
    Owned(u32),
    /// The one candidate type inherits the member (the heritage tier's).
    Inherited(u32),
    /// Exactly one of several same-named types owns it (the global tier's).
    Candidate(u32),
    /// Copies of one type own it; the copy nearest the caller.
    NearestCopy(u32),
    /// Several candidate types could own the member: no edge.
    Ambiguous,
    NoEdge,
}

/// What the ladder reads besides the pick facts.
pub trait TypeFacts: PickFacts {
    /// `(bases, first_unresolved)` of the type `ty`: resolved bases in
    /// declared order, and the number of resolved bases declared before the
    /// first unresolved one. `None` when `ty` declares no heritage.
    fn supertypes(&self, ty: u32) -> Option<(Cow<'_, [u32]>, Option<u32>)>;
    /// The name `id` is declared under.
    fn name_of(&self, id: u32) -> Option<&str>;
    /// Members named `member` in `file` owned by `owner`, in source order,
    /// of `target`'s kinds.
    fn owned_in_file(
        &self,
        file: &str,
        member: &str,
        owner: &str,
        target: ResolveTarget,
    ) -> Vec<u32>;
    /// Members named `member` owned by `owner` in the scope of `scope_file`:
    /// the same language, vendor barrier and crate root, and for Go the
    /// same directory.
    fn owned_global(
        &self,
        member: &str,
        owner: &str,
        target: ResolveTarget,
        scope_file: &str,
    ) -> Vec<u32>;
    /// Number of declarations named `name` in `file` of `target`'s kinds.
    fn count_in_file(&self, file: &str, name: &str, target: ResolveTarget) -> usize;
    /// Declarations named `name` of `target`'s kinds that `caller` admits.
    fn global_candidates(&self, name: &str, target: ResolveTarget, caller: FileMeta) -> Vec<u32>;
    /// Whether `b` repeats declaration `a` in another source tree.
    fn is_copy(&self, a: u32, b: u32) -> bool;
    /// The one of `candidates`, copies of one declaration, nearest
    /// `caller_file`; `None` for a tie or when one is no copy.
    fn nearest_copy(&self, caller_file: &str, candidates: &[u32]) -> Option<u32>;
    /// The return type the function `callee` declares, or the type the C#
    /// field or property `callee` declares, as its signature writes it or
    /// already reduced by [`return_type_key`]; `None` when it declares none
    /// or is neither.
    fn return_type_of(&self, callee: u32) -> Option<&str>;
    /// Whether a member lookup by name reaches neither the member `id` nor
    /// any later overload of it: C# explicit interface implementations
    /// (`bool IBag.Remove(object o)`). A lookup that meets `id` beside an
    /// ordinary later overload keeps `id`, and the access rule hands the
    /// call to that overload.
    fn hidden_member(&self, _id: u32) -> bool {
        false
    }
    /// Every part of `ty`'s C# `partial` type, the representative first,
    /// `ty` included, when the facts know the parts of one type; empty
    /// otherwise. A member another part declares is the type's own.
    fn partial_parts(&self, _ty: u32) -> Cow<'_, [u32]> {
        Cow::Borrowed(&[])
    }
    /// The node that stands for `ty`'s type: the representative part of a
    /// C# `partial` type, else `ty` itself. Two parts of one type are one
    /// candidate type.
    fn partial_type(&self, ty: u32) -> u32 {
        ty
    }
    /// The crate root of `path`: what precedes its last `/src/` or
    /// `/tests/` segment, else empty.
    fn crate_root<'p>(&self, path: &'p str) -> &'p str {
        path.rsplit_once("/src/")
            .or_else(|| path.rsplit_once("/tests/"))
            .map_or("", |(root, _)| root)
    }
}

/// Whether a declared type `key` names the type that declares the member in
/// `language`: Rust and Swift `Self`, PHP `self` / `static`, TypeScript
/// `this`, and Python `Self` (`typing.Self`). The text alone is not enough: a
/// C++ `struct Self {}` is an ordinary type.
pub fn names_declaring_type_in(language: Language, key: &str) -> bool {
    matches!(
        (language, key),
        (Language::Rust | Language::Swift | Language::Python, "Self")
            | (Language::Php, "self" | "static")
            | (Language::TypeScript | Language::JavaScript, "this")
    )
}

/// The type name a declared return type reduces to when only wrappers that
/// keep the type's members surround it: a leading `:` / `->` an extractor
/// kept, Python string quotes (`"Foo"`),
/// a union with a null (`Foo | null`, `Foo | undefined`, `Foo | None`), a
/// nullable marker (`?Foo`, `Foo?`, `Foo!`), a pointer or reference
/// (`*Foo`, `&Foo`, `&mut Foo`, `&'a Foo`, `Foo*`, `Foo&`), `const`, and a C
/// `struct` / C++ `class` tag. `None` for everything else: a generic or an
/// array (`Promise<Foo>`, `Optional<Foo>`, `Vec<Foo>`, `list[Foo]`,
/// `Foo[]`), a qualified path, a tuple, a function type, a union of two
/// real types. A guess would bind the wrong member of a receiver; edges to
/// those types read [`annotation_type_names`] instead.
pub fn return_type_key(raw: &str) -> Option<&str> {
    let mut ty = unquote(raw.trim());
    if ty.contains('|') {
        let mut kept = ty
            .split('|')
            .map(str::trim)
            .filter(|part| !matches!(*part, "null" | "undefined" | "None" | "nil"));
        ty = kept.next()?;
        if kept.next().is_some() {
            return None;
        }
    }
    loop {
        let before = ty;
        ty = ty.trim();
        for prefix in [
            "->", ":", "?", "*", "&", "const ", "mut ", "struct ", "class ",
        ] {
            ty = ty.strip_prefix(prefix).unwrap_or(ty).trim_start();
        }
        if let Some(lifetimed) = ty.strip_prefix('\'') {
            ty = lifetimed
                .split_once(char::is_whitespace)
                .map_or("", |(_, rest)| rest);
        }
        for suffix in ["?", "!", "*", "&", " const"] {
            ty = ty.strip_suffix(suffix).unwrap_or(ty).trim_end();
        }
        if ty == before {
            break;
        }
    }
    let names_a_type = !ty.is_empty()
        && !matches!(ty, "null" | "undefined" | "None" | "nil" | "void")
        && ty
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    names_a_type.then_some(ty)
}

/// `text` without one pair of surrounding `"` or `'` quotes.
fn unquote(text: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.trim();
        }
    }
    text
}

/// Words an annotation spells next to a type that are no type name: a
/// marker (`mut`, `const`, `dyn`, `impl`, `ref`, `out`), a tag (`struct`,
/// `class`), a modifier, or a literal keyword. Wrapper types (`Option`,
/// `List`) are absent on purpose: the resolver binds an unknown name to
/// nothing, so listing them would only add a hardcoded set.
fn is_annotation_keyword(word: &str) -> bool {
    matches!(
        word,
        "mut"
            | "const"
            | "dyn"
            | "impl"
            | "struct"
            | "class"
            | "enum"
            | "union"
            | "unsigned"
            | "signed"
            | "volatile"
            | "typename"
            | "readonly"
            | "keyof"
            | "typeof"
            | "extends"
            | "super"
            | "infer"
            | "asserts"
            | "new"
            | "fn"
            | "func"
            | "map"
            | "chan"
            | "interface"
            | "final"
            | "ref"
            | "out"
            | "in"
            | "is"
            | "as"
            | "inout"
            | "params"
            | "vararg"
            | "some"
            | "any"
            | "throws"
            | "async"
            | "escaping"
            | "where"
            | "unsafe"
            | "extern"
            | "void"
            | "null"
            | "undefined"
            | "None"
            | "nil"
    )
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Byte length of the name at the start of `text` (which starts with an
/// identifier character): the identifier, plus each `::`, `.` or `\`
/// separator that another identifier follows (`ns::St`, `types.St`,
/// `App\Models\St`).
fn qualified_name_len(text: &str) -> usize {
    let mut end = text.find(|c: char| !is_ident_char(c)).unwrap_or(text.len());
    loop {
        let tail = &text[end..];
        let sep = if tail.starts_with("::") {
            2
        } else if tail.starts_with(['.', '\\']) {
            1
        } else {
            break;
        };
        let after = &tail[sep..];
        if !after.starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '$') {
            break;
        }
        end += sep
            + after
                .find(|c: char| !is_ident_char(c))
                .unwrap_or(after.len());
    }
    end
}

/// Every type name a declared type annotation mentions, in source order:
/// generic arguments at any depth (`Map<String, List<St>>`), array, slice
/// and map parts (`[]pkg.St`, `St[]`, `[String: St]`), each member of a
/// union (`St | null`), a Python forward reference (`"St"`, `Optional["St"]`),
/// with nullable, pointer, reference, `mut`, `const`, `dyn`, `impl` and
/// lifetime markers dropped. A qualified name stays whole (`types.St`,
/// `ns::St`, `crate::t::St`): the resolver reads the qualifier, and a name
/// it cannot bind whole binds nothing, never a same-named type elsewhere.
///
/// A name that is no type stays out: a parameter name inside a function
/// type (Go `func(a St)`, Dart and C / C++ `(St a)`, a TS / Kotlin / Swift
/// label `(a: St)`), and a Python value inside `Literal[..]` or past the
/// first argument of `Annotated[..]` (`Literal["St"]` names a string).
///
/// The result is a superset: it holds wrappers (`Option`), primitives
/// (`int`) and a generic parameter (`T`). The resolver binds the names that
/// are project types and nothing else. A declaring-type keyword (`Self`,
/// `this`) is no name; the caller maps it to the owner through
/// [`return_type_key`]. [`return_type_key`] stays the reduction that types a
/// receiver; this one feeds edges.
pub fn annotation_type_names(language: Language, raw: &str) -> Vec<&str> {
    /// One name the scan met, with the bracket group it sits in directly.
    struct Word<'a> {
        text: &'a str,
        /// The `(` or `{` group (numbered in open order) and its comma
        /// segment, when that group is the innermost bracket.
        slot: Option<(usize, u32)>,
        labelled: bool,
        keyword: bool,
    }
    struct Open {
        group: Option<usize>,
        segment: u32,
        /// The first segment that holds values, not types: Python
        /// `Literal[` 0, `Annotated[` 1.
        value_from: Option<u32>,
    }
    let labels = matches!(
        language,
        Language::TypeScript | Language::JavaScript | Language::Kotlin | Language::Swift
    );
    let mut words: Vec<Word<'_>> = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    let mut groups = 0usize;
    let mut last_word: Option<(&str, usize)> = None;
    let mut rest = raw;
    while let Some(c) = rest.chars().next() {
        let at = raw.len() - rest.len();
        let in_value = stack
            .iter()
            .any(|open| open.value_from.is_some_and(|from| open.segment >= from));
        let skip = if c.is_alphabetic() || c == '_' || c == '$' {
            let len = qualified_name_len(rest);
            let text = &rest[..len];
            if !in_value {
                let slot = stack
                    .last()
                    .and_then(|open| Some((open.group?, open.segment)));
                let tail = rest[len..].trim_start();
                let tail = tail.strip_prefix('?').map_or(tail, str::trim_start);
                words.push(Word {
                    text,
                    slot,
                    labelled: labels
                        && slot.is_some()
                        && tail.starts_with(':')
                        && !tail.starts_with("::"),
                    keyword: is_annotation_keyword(text) || names_declaring_type_in(language, text),
                });
            }
            last_word = Some((text, at + len));
            len
        } else if c.is_ascii_digit() {
            rest.find(|ch: char| !is_ident_char(ch))
                .unwrap_or(rest.len())
        } else if language == Language::Rust && c == '\'' {
            // A lifetime (`'a`, `'static`): the quote and its one identifier.
            let after = &rest[1..];
            1 + after
                .find(|ch: char| !is_ident_char(ch))
                .unwrap_or(after.len())
        } else if matches!(c, '"' | '\'') && (language != Language::Python || in_value) {
            // A string literal type (`"admin"`) or a Python literal value
            // names no type; any other Python quoted text is a forward
            // reference and is read as code.
            rest[1..].find(c).map_or(1, |close| close + 2)
        } else if rest.starts_with("->")
            || rest.starts_with("=>")
            || (language == Language::Go && rest.starts_with("<-"))
        {
            // An arrow (or a Go channel direction) closes no bracket.
            2
        } else {
            match c {
                '(' | '{' | '[' | '<' => {
                    let value_from = (language == Language::Python && c == '[')
                        .then_some(last_word)
                        .flatten()
                        .filter(|&(_, end)| end == at)
                        .and_then(|(word, _)| match word.rsplit('.').next() {
                            Some("Literal") => Some(0),
                            Some("Annotated") => Some(1),
                            _ => None,
                        });
                    let group = matches!(c, '(' | '{').then(|| {
                        groups += 1;
                        groups - 1
                    });
                    stack.push(Open {
                        group,
                        segment: 0,
                        value_from,
                    });
                }
                ')' | '}' | ']' | '>' => {
                    stack.pop();
                }
                ',' | ';' => {
                    if let Some(open) = stack.last_mut() {
                        open.segment += 1;
                    }
                }
                _ => {}
            }
            c.len_utf8()
        };
        rest = &rest[skip..];
    }
    let mut dropped = vec![false; words.len()];
    for (i, word) in words.iter().enumerate() {
        if word.labelled {
            // A label and any external label before it (Swift `(_ a: St)`).
            for (j, earlier) in words[..=i].iter().enumerate() {
                if earlier.slot == word.slot {
                    dropped[j] = true;
                }
            }
        }
    }
    let mut segments: Vec<((usize, u32), Vec<usize>)> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let Some(slot) = word.slot else { continue };
        match segments.iter_mut().find(|(seen, _)| *seen == slot) {
            Some((_, members)) => members.push(i),
            None => segments.push((slot, vec![i])),
        }
    }
    match language {
        // `func(a St)`, `func(a, b St)`: a name leads each parameter, and in
        // a list where one parameter has a name, every one has. A type
        // keyword (`func`, `map`, `chan`) leads a type instead.
        Language::Go => {
            let named = |members: &[usize]| members.len() > 1 && !words[members[0]].keyword;
            let named_groups: Vec<usize> = segments
                .iter()
                .filter(|(_, members)| named(members.as_slice()))
                .map(|((group, _), _)| *group)
                .collect();
            for ((group, _), members) in &segments {
                if named_groups.contains(group) && !words[members[0]].keyword {
                    dropped[members[0]] = true;
                }
            }
        }
        // `(St a)`, `(const struct St *a)`: a name ends a parameter.
        Language::Dart | Language::C | Language::Cpp => {
            for (_, members) in &segments {
                let mut types = members.iter().filter(|&&i| !words[i].keyword);
                if let (Some(_), Some(_)) = (types.next(), types.next()) {
                    if let Some(&last) = members.iter().rev().find(|&&i| !words[i].keyword) {
                        dropped[last] = true;
                    }
                }
            }
        }
        _ => {}
    }
    words
        .iter()
        .zip(dropped)
        .filter(|(word, dropped)| !word.keyword && !dropped)
        .map(|(word, _)| word.text)
        .collect()
}

/// The reason of the `Accesses` edge from a declaration to a type its
/// annotation names.
pub const TYPE_ANNOTATION_REASON: &str = "type_annotation";

/// The `(target, confidence)` of every `type_annotation` `Accesses` edge of
/// one declaration, in emit order: the index's Pass 2 rule, which the
/// overlay replays for a dirty declaration through this same function.
/// `resolve` is the declaring file's type lookup (`ResolveTarget::Type`):
/// it yields every target a name binds (the index passes all of them, the
/// overlay at most one).
///
/// - `type_annotation` (`RawNode.type_annotation`) binds first as one
///   name ([`return_type_key`]): a declaring-type keyword
///   (`Self`, `this`, PHP `static`) stands for `owner_class`, so it binds
///   nothing without an owner; any other text the key does not reduce
///   is looked up whole.
/// - A function, method or constructor then binds every name its
///   generic or container return mentions ([`annotation_type_names`]):
///   the annotation's when the key does not reduce it, else nothing, and
///   `return_type` (its function meta's) when the parse captured no
///   annotation. Then every type of `params`, the meta's flat
///   `[name, type, ...]` list. A target bound already is not bound again.
pub fn type_annotation_targets<'a, T: IntoIterator<Item = (u32, f32)>>(
    language: Language,
    kind: NodeKind,
    type_annotation: Option<&'a str>,
    owner_class: Option<&'a str>,
    return_type: Option<&'a str>,
    params: &'a [String],
    mut resolve: impl FnMut(&'a str) -> T,
) -> Vec<(u32, f32)> {
    let mut targets: Vec<(u32, f32)> = Vec::new();
    if let Some(annotation) = type_annotation {
        let name = match return_type_key(annotation) {
            Some(key) if names_declaring_type_in(language, key) => owner_class.and_then(owner_key),
            Some(key) => Some(key),
            None => Some(annotation),
        };
        targets.extend(name.into_iter().flat_map(&mut resolve));
    }
    if !kind.is_callable() {
        return targets;
    }
    let returned = match type_annotation {
        Some(annotation) if return_type_key(annotation).is_none() => Some(annotation),
        Some(_) => None,
        None => return_type,
    };
    let texts = returned
        .into_iter()
        .chain(params.as_chunks::<2>().0.iter().map(|[_, ty]| ty.as_str()));
    for text in texts {
        for name in annotation_type_names(language, text) {
            for (target, confidence) in resolve(name) {
                if targets.iter().all(|&(bound, _)| bound != target) {
                    targets.push((target, confidence));
                }
            }
        }
    }
    targets
}

/// The name of the type a call to `callee` returns, as the callee's own
/// file names it: a constructor's owner, a constructible type itself (the
/// constructor fallback lands on a type that declares no constructor), or
/// the function's declared return type reduced by [`return_type_key`],
/// where `Self` / `self` / `static` / `this` stand for the declaring type.
/// The receiver of a chained call (`Foo.create().bar()`) has this type. A
/// C# field or property (`_reader.Read()`) returns the type it declares.
pub fn returned_type<F: TypeFacts + ?Sized>(facts: &F, callee: u32) -> Option<&str> {
    match facts.kind(callee) {
        NodeKind::Constructor => facts.owner(callee),
        kind if kind.is_constructible() => facts.name_of(callee),
        NodeKind::Function | NodeKind::Method => {
            let key = return_type_key(facts.return_type_of(callee)?)?;
            let language = facts
                .file_of(callee)
                .map(|file| facts.file_meta(file).language);
            if language.is_some_and(|language| names_declaring_type_in(language, key)) {
                facts.owner(callee)
            } else {
                Some(key)
            }
        }
        NodeKind::Property => return_type_key(facts.return_type_of(callee)?),
        _ => None,
    }
}

/// The field or property `name` of the C# type `ty` (declared as
/// `ty_name`) that a bare receiver `name.M()` in a member of `ty` reads,
/// when the parser could not type it: one declared in another `partial`
/// part, or inherited ([`member_ownership`] reads every part and the
/// bases). `None` when no such member exists, when two parts declare one
/// (C# rejects it, and the index cannot tell which the call reads), or
/// when the lookup is unsure.
pub fn csharp_member_field<F: TypeFacts + ?Sized>(
    facts: &F,
    ty: u32,
    ty_name: &str,
    name: &str,
) -> Option<u32> {
    // The C# parser writes `this/name` for a `this.name` it cannot see.
    let name = name.strip_prefix(THIS_MEMBER_PREFIX).unwrap_or(name);
    let Ownership::Owned { id, inherited } =
        member_ownership(facts, ty, ty_name, name, ResolveTarget::Field)
    else {
        return None;
    };
    let parts = facts.partial_parts(ty);
    let mut part_files: Vec<&str> = parts
        .iter()
        .filter_map(|&part| facts.file_of(part))
        .collect();
    part_files.sort_unstable();
    part_files.dedup();
    let declared: usize = part_files
        .iter()
        .map(|file| {
            facts
                .owned_in_file(file, name, ty_name, ResolveTarget::Field)
                .len()
        })
        .sum();
    (inherited || declared <= 1).then_some(id)
}

/// Select a visible return type before inspecting its members. An ambiguous
/// type remains ambiguous even when only one candidate declares the member.
pub fn visible_return_type<'q, F: TypeFacts + ?Sized>(
    facts: &F,
    source: &str,
    name: &'q str,
    visible: impl FnMut(u32) -> bool,
) -> TypeCandidates<'q> {
    let candidates = facts.global_candidates(name, ResolveTarget::Type, facts.file_meta(source));
    visible_level(facts, &candidates, name, visible).unwrap_or(TypeCandidates::None)
}

/// [`visible_return_type`] for a C# return type `name` read in `source`,
/// whose file declares `source_namespaces`. Level 0 is `visible`, today's
/// rule ([`namespace_return_type_visible`]). C# also searches every
/// namespace that encloses the source's one namespace: when level 0 admits
/// no candidate, each enclosing namespace is a level, innermost first, and
/// `in_namespace(id, namespace)` says whether candidate `id` is declared
/// there. The first level with a candidate decides: one type binds, two or
/// more bind none. Level 0 keeps its rank over an enclosing namespace, so
/// no edge level 0 gives is lost (C# would rank the enclosing namespace
/// above a `using`).
pub fn csharp_visible_return_type<'q, 'n, F: TypeFacts + ?Sized>(
    facts: &F,
    source: &str,
    name: &'q str,
    source_namespaces: impl Iterator<Item = &'n str>,
    mut visible: impl FnMut(u32) -> bool,
    mut in_namespace: impl FnMut(u32, &str) -> bool,
) -> TypeCandidates<'q> {
    let candidates = facts.global_candidates(name, ResolveTarget::Type, facts.file_meta(source));
    if let Some(found) = visible_level(facts, &candidates, name, &mut visible) {
        return found;
    }
    let Some(namespace) = declared_namespace(None, source_namespaces) else {
        return TypeCandidates::None;
    };
    enclosing_namespaces(namespace)
        .find_map(|outer| visible_level(facts, &candidates, name, |id| in_namespace(id, outer)))
        .unwrap_or(TypeCandidates::None)
}

/// The namespaces that enclose `namespace` (`.`-joined), innermost first.
/// The global namespace is not one of them.
fn enclosing_namespaces(namespace: &str) -> impl Iterator<Item = &str> {
    fn outer(ns: &str) -> Option<&str> {
        ns.rsplit_once('.').map(|(outer, _)| outer)
    }
    std::iter::successors(outer(namespace), |&ns| outer(ns))
}

/// The type among `candidates` that `admits` keeps, parts of one partial
/// type read as one: `None` when it keeps none, [`TypeCandidates::None`]
/// when it keeps two types.
fn visible_level<'q, F: TypeFacts + ?Sized>(
    facts: &F,
    candidates: &[u32],
    name: &'q str,
    mut admits: impl FnMut(u32) -> bool,
) -> Option<TypeCandidates<'q>> {
    let mut types = distinct_types(facts, candidates.iter().copied().filter(|&id| admits(id)));
    match (types.next(), types.next()) {
        (None, _) => None,
        (Some(id), None) => Some(TypeCandidates::Unique(id, name)),
        (Some(_), Some(_)) => Some(TypeCandidates::None),
    }
}

/// Namespace visible from a type's owner key. A file-scoped namespace has
/// no stamped owner, so it requires one namespace in the file. Two full
/// namespace names with one owner key cannot be distinguished.
pub fn declared_namespace<'a>(
    owner: Option<&str>,
    namespaces: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    let mut names = namespaces.filter(|name| {
        owner.is_none_or(|owner| crate::analyzer::types::owner_key(name) == Some(owner))
    });
    match (names.next(), names.next()) {
        (None, None) if owner.is_none() => Some(""),
        (Some(name), None) => Some(name),
        _ => None,
    }
}

/// Whether a C# type belongs to the global namespace, the callee's one
/// namespace, or an explicitly imported namespace.
pub fn namespace_return_type_visible<'a>(
    owner: Option<&str>,
    namespaces: impl Iterator<Item = &'a str>,
    source_namespaces: impl Iterator<Item = &'a str>,
    imports: &[RawImport],
) -> bool {
    let Some(namespace) = declared_namespace(owner, namespaces) else {
        return false;
    };
    namespace.is_empty()
        || declared_namespace(None, source_namespaces) == Some(namespace)
        || imports.iter().any(|import| {
            import.alias.is_none()
                && import.source == namespace
                && import.imported_name == namespace
        })
}

/// Whether a type's namespace agrees with a C# alias's full type path.
pub fn namespace_type_path_matches<'a>(
    owner: Option<&str>,
    namespaces: impl Iterator<Item = &'a str>,
    path: &str,
) -> bool {
    let path = path.strip_prefix("global::").unwrap_or(path);
    let namespace = path.rsplit_once('.').map_or("", |(namespace, _)| namespace);
    declared_namespace(owner, namespaces) == Some(namespace)
}

/// A Swift package target's conventional `Sources/<module>/` directory.
/// Custom target paths have no module evidence here.
fn swift_module(file: &str) -> Option<(&str, &str)> {
    let (root, tail) = file
        .rsplit_once("/Sources/")
        .or_else(|| file.strip_prefix("Sources/").map(|tail| ("", tail)))?;
    let (module, _) = tail.split_once('/')?;
    Some((root, module))
}

/// A Swift return type is visible in its own target or an imported target
/// of the same package. Without target layout evidence, preserve directory
/// scope rather than guessing a module from a global namesake.
pub fn swift_return_type_visible(source: &str, candidate: &str, imports: &[RawImport]) -> bool {
    match (swift_module(source), swift_module(candidate)) {
        (Some((root, module)), Some((other_root, other_module))) => {
            root == other_root
                && (module == other_module
                    || imports
                        .iter()
                        .any(|import| import.alias.is_none() && import.source == other_module))
        }
        _ => Path::new(source).parent() == Path::new(candidate).parent(),
    }
}

/// Languages where a field or class attribute hides an inherited method of
/// the same name: Python class attributes, JavaScript / TypeScript instance
/// fields over prototype methods, and Go fields over promoted methods. Java,
/// Kotlin, C#, Rust and the rest keep fields and methods apart.
pub fn attributes_shadow_methods(meta: FileMeta) -> bool {
    matches!(
        meta.language,
        Language::Python | Language::JavaScript | Language::TypeScript | Language::Go
    )
}

/// The ladder past step 1: resolve `member` of the types `candidates`
/// names. It emits an edge only when that type or one of its resolved
/// declared supertypes owns the member, and no unresolved base can shadow
/// the owner. It never matches the member name alone. `call` admits the
/// nearest-copy pick among several owning types.
pub fn member_via_type<F: TypeFacts + ?Sized>(
    facts: &F,
    candidates: TypeCandidates<'_>,
    caller_file: &str,
    member: &str,
    target: ResolveTarget,
    call: bool,
) -> TypeMember {
    match candidates {
        TypeCandidates::Unique(ty, ty_name) => {
            match member_ownership(facts, ty, ty_name, member, target) {
                Ownership::Owned {
                    id,
                    inherited: false,
                } => TypeMember::Owned(id),
                Ownership::Owned {
                    id,
                    inherited: true,
                } => TypeMember::Inherited(id),
                Ownership::NotOwned | Ownership::Unknown => TypeMember::NoEdge,
            }
        }
        TypeCandidates::Set(types, ty_name) => {
            let mut owner_members: Vec<u32> = Vec::new();
            for ty in types {
                match member_ownership(facts, ty, ty_name, member, target) {
                    Ownership::Owned { id, .. } => match owner_members.first() {
                        Some(_) if owner_members.contains(&id) => {}
                        Some(&first) if !call || !facts.is_copy(first, id) => {
                            return TypeMember::Ambiguous;
                        }
                        _ => owner_members.push(id),
                    },
                    Ownership::NotOwned => {}
                    Ownership::Unknown => return TypeMember::Ambiguous,
                }
            }
            match owner_members.as_slice() {
                [] => TypeMember::NoEdge,
                &[id] => TypeMember::Candidate(id),
                owners => match facts.nearest_copy(caller_file, owners) {
                    Some(id) => TypeMember::NearestCopy(id),
                    None => TypeMember::Ambiguous,
                },
            }
        }
        TypeCandidates::External | TypeCandidates::None => TypeMember::NoEdge,
    }
}

/// Ladder steps 2 and 3: does type `ty` own `member`, directly or through
/// its resolved declared supertypes?
///
/// The walk visits every ancestor (breadth-first, declared order, depth
/// [`MAX_HERITAGE_DEPTH`]). The owner is the most derived owning type; two
/// owners on separate branches depend on the language's method resolution
/// order, so they give `Unknown`. The result is also `Unknown` when
/// something the lookup may meet before the owner could supply the member
/// instead: an unresolved base, or a same-named attribute (`greet =
/// replacement`) where attributes hide methods, on the path to the owner,
/// on a branch declared before the owner's, or on a branch that also
/// reaches the owner.
pub fn member_ownership<F: TypeFacts + ?Sized>(
    facts: &F,
    ty: u32,
    ty_name: &str,
    member: &str,
    target: ResolveTarget,
) -> Ownership {
    match owned_member(facts, ty, ty_name, member, target) {
        MemberLookup::Hit(id) => {
            return Ownership::Owned {
                id,
                inherited: false,
            }
        }
        MemberLookup::Ambiguous => return Ownership::Unknown,
        MemberLookup::Miss => {}
    }
    let meta = facts.file_meta(facts.file_of(ty).unwrap_or_default());
    let shadows = attributes_shadow_methods(meta);
    // How each ancestor was first reached: (the type, base position).
    let mut parent: rustc_hash::FxHashMap<u32, (u32, u32)> = rustc_hash::FxHashMap::default();
    let mut frontier = vec![ty];
    // (type, number of resolved bases it declares before an unresolved one)
    let mut unresolved: Vec<(u32, u32)> = Vec::new();
    // Types declaring a same-named attribute, which hides a method.
    let mut attributes: Vec<u32> = Vec::new();
    if shadows && declares_attribute(facts, ty, ty_name, member) {
        attributes.push(ty);
    }
    let mut hits: Vec<(u32, u32)> = Vec::new();
    for _ in 0..MAX_HERITAGE_DEPTH {
        let mut next = Vec::new();
        for &current in &frontier {
            let Some((bases, first_unresolved)) = facts.supertypes(current) else {
                continue;
            };
            if let Some(at) = first_unresolved {
                unresolved.push((current, at));
            }
            for (pos, &base) in bases.iter().enumerate() {
                if base == ty || parent.contains_key(&base) {
                    continue;
                }
                parent.insert(base, (current, pos as u32));
                next.push(base);
                let Some(base_name) = facts.name_of(base) else {
                    continue;
                };
                if shadows && declares_attribute(facts, base, base_name, member) {
                    attributes.push(base);
                }
                match owned_member(facts, base, base_name, member, target) {
                    MemberLookup::Hit(id) => hits.push((base, id)),
                    MemberLookup::Ambiguous => return Ownership::Unknown,
                    MemberLookup::Miss => {}
                }
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    // The depth cap cut the walk short: a deeper base may own the member.
    if frontier.iter().any(|&t| facts.supertypes(t).is_some()) {
        return Ownership::Unknown;
    }
    // A C# class or struct reaches an interface's member only through its
    // own implementation, which its lookup meets first, or not at all (a
    // default interface method is called through the interface).
    if meta.language == Language::CSharp && facts.kind(ty) != NodeKind::Interface {
        hits.retain(|&(owner, _)| facts.kind(owner) != NodeKind::Interface);
    }
    let most_derived = hits.iter().find(|&&(owner, _)| {
        let above = facts.ancestors(owner);
        hits.iter()
            .all(|&(other, _)| other == owner || above.contains(&other))
    });
    let Some(&(owner, id)) = most_derived else {
        return if hits.is_empty() && unresolved.is_empty() {
            Ownership::NotOwned
        } else {
            Ownership::Unknown
        };
    };
    // The walk path from `ty` to the owner: each type on it, with the
    // position of the base it takes toward the owner.
    let mut path: rustc_hash::FxHashMap<u32, u32> = rustc_hash::FxHashMap::default();
    let mut node = owner;
    while let Some(&(up, pos)) = parent.get(&node) {
        path.insert(up, pos);
        node = up;
    }
    let above_owner = facts.ancestors(owner);
    // Does lookup meet `holder`'s contribution before the owner? `slot` is
    // an unresolved base of `holder` with `at` resolved bases before it;
    // `None` is a member `holder` declares itself. Anything above the owner
    // is hidden by it. Off the path, a holder that also reaches the owner
    // puts the owner after its own bases (a diamond: C3 visits a shared
    // ancestor last); otherwise its branch comes first when declared before
    // the branch the path takes.
    let before_owner = |holder: u32, slot: Option<u32>| {
        if holder == owner || above_owner.contains(&holder) {
            return false;
        }
        let (mut node, mut pos) = match (path.get(&holder), slot) {
            (Some(&taken), Some(at)) => return at <= taken,
            (Some(_), None) => return true,
            (None, _) if facts.ancestors(holder).contains(&owner) => return true,
            (None, _) => match parent.get(&holder) {
                Some(&(up, at)) => (up, at),
                None => return true,
            },
        };
        loop {
            if let Some(&taken) = path.get(&node) {
                return pos < taken;
            }
            let Some(&(up, at)) = parent.get(&node) else {
                return true;
            };
            (node, pos) = (up, at);
        }
    };
    if unresolved.iter().any(|&(h, at)| before_owner(h, Some(at)))
        || attributes.iter().any(|&h| before_owner(h, None))
    {
        return Ownership::Unknown;
    }
    Ownership::Owned {
        id,
        inherited: true,
    }
}

/// True when type `ty` declares a non-callable member named `member` (a
/// field or a class attribute), which hides an inherited method in the
/// languages [`attributes_shadow_methods`] lists.
fn declares_attribute<F: TypeFacts + ?Sized>(
    facts: &F,
    ty: u32,
    ty_name: &str,
    member: &str,
) -> bool {
    facts.file_of(ty).is_some_and(|file| {
        !facts
            .owned_in_file(file, member, ty_name, ResolveTarget::Field)
            .is_empty()
    })
}

/// The member of `ty` named `member`, by the owner index: first in the
/// type's own file, then in the type's crate / package scope.
pub fn owned_member<F: TypeFacts + ?Sized>(
    facts: &F,
    ty: u32,
    ty_name: &str,
    member: &str,
    target: ResolveTarget,
) -> MemberLookup {
    let Some(ty_file) = facts.file_of(ty) else {
        return MemberLookup::Miss;
    };
    let owner_target = if facts.file_meta(ty_file).language == Language::Go {
        ResolveTarget::Type
    } else {
        ResolveTarget::Qualifier
    };
    let declares_owner = |file: &str| facts.count_in_file(file, ty_name, owner_target);
    // The type's own file first, then the files of its other C# partial
    // parts: a member any part declares is the type's own.
    let parts = facts.partial_parts(ty);
    let part_files = parts
        .iter()
        .filter_map(|&part| facts.file_of(part))
        .filter(|&file| file != ty_file);
    for file in std::iter::once(ty_file).chain(part_files) {
        let mut in_file = facts.owned_in_file(file, member, ty_name, target);
        in_file.retain(|&id| !facts.hidden_member(id));
        if let Some(&first) = in_file.first() {
            // Several hits are overloads of the one type, unless the file
            // declares two owners of that name (nested types, a type and a
            // module); the owner index cannot tell those apart.
            return if in_file.len() == 1 || declares_owner(file) == 1 {
                MemberLookup::Hit(first)
            } else {
                MemberLookup::Ambiguous
            };
        }
    }
    let mut elsewhere = facts.owned_global(member, ty_name, target, ty_file);
    elsewhere.retain(|&id| !facts.hidden_member(id));
    let Some(&first) = elsewhere.first() else {
        return MemberLookup::Miss;
    };
    if type_unique_in_scope(facts, ty_name, ty_file) {
        return MemberLookup::Hit(first);
    }
    // Another same-named type shares the scope. Where a type can be
    // reopened in another file (Ruby `class Derived`, a C# partial class, a
    // Swift / Dart extension), a member there may be this type's own;
    // elsewhere a member in a file declaring that name belongs to that
    // other type, and any other member cannot be attributed.
    let reopenable = matches!(
        facts.file_meta(ty_file).language,
        Language::Ruby | Language::Crystal | Language::CSharp | Language::Swift | Language::Dart
    );
    if !reopenable
        && elsewhere
            .iter()
            .all(|&id| facts.file_of(id).is_some_and(|f| declares_owner(f) > 0))
    {
        MemberLookup::Miss
    } else {
        MemberLookup::Ambiguous
    }
}

/// True when `ty_file`'s type `ty_name` is the only qualifier of that name
/// in the scope [`TypeFacts::owned_global`] searches: the same language and
/// vendor barrier and crate root, and for Go one package.
pub fn type_unique_in_scope<F: TypeFacts + ?Sized>(
    facts: &F,
    ty_name: &str,
    ty_file: &str,
) -> bool {
    let meta = facts.file_meta(ty_file);
    let root = facts.crate_root(ty_file);
    let dir = Path::new(ty_file).parent();
    let same_package_only = meta.language == Language::Go;
    let owner_target = if same_package_only {
        ResolveTarget::Type
    } else {
        ResolveTarget::Qualifier
    };
    facts
        .global_candidates(ty_name, owner_target, meta)
        .into_iter()
        .filter(|&id| {
            facts.file_of(id).is_some_and(|f| {
                facts.crate_root(f) == root && (!same_package_only || Path::new(f).parent() == dir)
            })
        })
        .take(2)
        .count()
        == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `type_annotation_targets` over a resolver that knows `names`, each
    /// at its index + 1; the names it was asked for, in order.
    fn annotation_targets(
        language: Language,
        kind: NodeKind,
        annotation: Option<&str>,
        owner: Option<&str>,
        return_type: Option<&str>,
        params: &[&str],
        names: &[&str],
    ) -> (Vec<u32>, Vec<String>) {
        let params: Vec<String> = params.iter().map(|p| p.to_string()).collect();
        let mut asked = Vec::new();
        let targets = type_annotation_targets(
            language,
            kind,
            annotation,
            owner,
            return_type,
            &params,
            |name| {
                asked.push(name.to_string());
                names
                    .iter()
                    .position(|known| *known == name)
                    .map(|at| (at as u32 + 1, 1.0))
            },
        );
        (targets.into_iter().map(|(id, _)| id).collect(), asked)
    }

    /// Stands for: the index's Pass 2 rule, row by row. A declaring-type
    /// keyword binds the owner and only the owner; a field binds its whole
    /// annotation and never the generic arguments in it; a callable binds a
    /// bare return once, every name of a generic return, the meta's return
    /// only when the parse captured no annotation, and each parameter
    /// type, one edge per target.
    #[test]
    fn test_type_annotation_targets_rows_match_the_index_rule() {
        use Language::{Rust, Swift, TypeScript};
        use NodeKind::{Function, Method, Property};
        let st = &["St", "Other", "Owner"][..];
        type Row<'r> = (
            &'r str,
            (
                Language,
                NodeKind,
                Option<&'r str>,
                Option<&'r str>,
                Option<&'r str>,
            ),
            &'r [&'r str],
            &'r [u32],
        );
        let rows: [Row; 10] = [
            (
                "bare return",
                (Rust, Function, Some("St"), None, Some("St")),
                &[],
                &[1],
            ),
            (
                "Self return",
                (Rust, Method, Some("Self"), Some("Owner"), Some("Self")),
                &[],
                &[3],
            ),
            (
                "Self without owner",
                (Rust, Function, Some("Self"), None, None),
                &[],
                &[],
            ),
            (
                "Self inside a generic",
                (Rust, Method, Some("Option<Self>"), Some("Owner"), None),
                &[],
                &[],
            ),
            (
                "generic return",
                (Rust, Function, Some("Option<St>"), None, None),
                &[],
                &[1],
            ),
            (
                "uncaptured return reads the meta",
                (TypeScript, Function, None, None, Some("Promise<Other>")),
                &[],
                &[2],
            ),
            (
                "captured bare return skips the meta",
                (
                    TypeScript,
                    Function,
                    Some("St"),
                    None,
                    Some("Promise<Other>"),
                ),
                &[],
                &[1],
            ),
            (
                "generic field binds no argument",
                (Swift, Property, Some("[St]"), None, None),
                &[],
                &[],
            ),
            (
                "params bind once per target",
                (Rust, Function, Some("St"), None, None),
                &["a", "St", "b", "&St", "c", "Vec<Other>"],
                &[1, 2],
            ),
            (
                "receiver and untyped params",
                (Rust, Method, None, Some("Owner"), None),
                &["self", "", "a", ""],
                &[],
            ),
        ];
        for (label, (language, kind, annotation, owner, return_type), params, want) in rows {
            let (got, _) =
                annotation_targets(language, kind, annotation, owner, return_type, params, st);
            assert_eq!(got, want, "{label}");
        }
    }

    /// Stands for: the whole annotation text is looked up first, as the
    /// index looks it up, so a qualified spelling resolves through its
    /// qualifier before its parts are read.
    #[test]
    fn test_type_annotation_targets_unreduced_text_looked_up_whole_first() {
        let (_, asked) = annotation_targets(
            Language::Rust,
            NodeKind::Function,
            Some("Option<St>"),
            None,
            None,
            &[],
            &[],
        );
        assert_eq!(asked, ["Option<St>", "Option", "St"]);
    }

    /// Contract: a parameter name inside a function type is no type, in each
    /// language whose annotation text can hold one: Go names lead a
    /// parameter (and a list with one named parameter names them all), Dart
    /// and C / C++ names end it, a TS / JS / Kotlin / Swift label precedes a
    /// `:`. A type keyword that leads a Go type (`func`, `map`, `chan`) is no
    /// name, and a Swift dictionary key (`[String: St]`) stays a type.
    #[test]
    fn test_annotation_type_names_function_type_parameter_names_excluded() {
        use Language::*;
        let rows: &[(Language, &str, &[&str])] = &[
            (Go, "func(a St)", &["St"]),
            (Go, "func(a, b St) error", &["St", "error"]),
            (Go, "func(St, Other)", &["St", "Other"]),
            (Go, "func(cb func(x St))", &["St"]),
            (Go, "func(m map[K]V, c chan St)", &["K", "V", "St"]),
            (
                Go,
                "func(a ...St) (n int, err error)",
                &["St", "int", "error"],
            ),
            (Go, "func(ch <-chan St, b int)", &["St", "int"]),
            (Go, "struct { a St; b Other }", &["St", "Other"]),
            (Dart, "void Function(St a)", &["Function", "St"]),
            (
                Dart,
                "void Function(St, {required Other b})",
                &["Function", "St", "required", "Other"],
            ),
            (
                Dart,
                "void Function(Map<K, V> m)",
                &["Function", "Map", "K", "V"],
            ),
            (C, "void (*)(const struct St *a)", &["St"]),
            (C, "int (*)(St, Other)", &["int", "St", "Other"]),
            (
                Cpp,
                "std::function<void(const St& a)>",
                &["std::function", "St"],
            ),
            (
                Cpp,
                "std::function<void(const St&)>",
                &["std::function", "St"],
            ),
            (TypeScript, "(a: St, b?: Other) => void", &["St", "Other"]),
            (TypeScript, "(...rest: St[]) => Out", &["St", "Out"]),
            (TypeScript, "{ a: St; b: Other }", &["St", "Other"]),
            (JavaScript, "(a: St) => Out", &["St", "Out"]),
            (Kotlin, "(a: St) -> Unit", &["St", "Unit"]),
            (
                Kotlin,
                "Map<String, (a: St) -> Unit>",
                &["Map", "String", "St", "Unit"],
            ),
            (Swift, "(_ a: St) -> Void", &["St", "Void"]),
            (Swift, "(St) -> [String: Other]", &["St", "String", "Other"]),
            (Rust, "fn(St) -> Out", &["St", "Out"]),
            (Rust, "(St, Other)", &["St", "Other"]),
            (Python, "Callable[[St], Out]", &["Callable", "St", "Out"]),
        ];
        for (language, raw, want) in rows {
            assert_eq!(
                annotation_type_names(*language, raw),
                *want,
                "{language:?} {raw:?}"
            );
        }
    }

    /// Contract: a Python quoted string is a forward reference only where a
    /// type is expected. `Literal[..]` holds values, and `Annotated[..]`
    /// holds a type first and metadata after it, so their strings (and the
    /// metadata's names) name no type.
    #[test]
    fn test_annotation_type_names_python_literal_values_excluded() {
        let rows: &[(&str, &[&str])] = &[
            ("Literal[\"St\"]", &["Literal"]),
            ("typing.Literal['St', 'Other']", &["typing.Literal"]),
            ("Optional[Literal[\"St\"]]", &["Optional", "Literal"]),
            ("Annotated[St, \"Other\"]", &["Annotated", "St"]),
            ("Annotated[\"St\", Gt(0)]", &["Annotated", "St"]),
            (
                "Annotated[list[St], \"meta\"] | Other",
                &["Annotated", "list", "St", "Other"],
            ),
            ("dict[str, \"St\"]", &["dict", "str", "St"]),
            ("Literal []", &["Literal"]),
            ("Literal[]", &["Literal"]),
        ];
        for (raw, want) in rows {
            assert_eq!(
                annotation_type_names(Language::Python, raw),
                *want,
                "{raw:?}"
            );
        }
        // Outside Python a `Literal` is an ordinary generic.
        assert_eq!(
            annotation_type_names(Language::TypeScript, "Literal<St>"),
            ["Literal", "St"]
        );
    }

    /// Contract: only wrappers that keep the type's members reduce; a
    /// generic, an array, a path or a two-type union stays unresolved (the
    /// follow-up), so no chained call guesses its receiver.
    #[test]
    fn test_return_type_key_wrappers_and_generics_returns_bare_name_or_none() {
        for (raw, want) in [
            ("Foo", Some("Foo")),
            ("  Foo ", Some("Foo")),
            (": Foo", Some("Foo")),
            ("-> Foo", Some("Foo")),
            ("\"Foo\"", Some("Foo")),
            ("'Foo'", Some("Foo")),
            ("Foo | null", Some("Foo")),
            ("undefined | Foo", Some("Foo")),
            ("Foo | None", Some("Foo")),
            ("\"Foo | None\"", Some("Foo")),
            ("?Foo", Some("Foo")),
            ("Foo?", Some("Foo")),
            ("Foo!", Some("Foo")),
            ("*Foo", Some("Foo")),
            ("&Foo", Some("Foo")),
            ("&mut Self", Some("Self")),
            ("&'a Foo", Some("Foo")),
            ("const Foo&", Some("Foo")),
            ("Foo*", Some("Foo")),
            ("struct Foo", Some("Foo")),
            ("*const Foo", Some("Foo")),
            ("static", Some("static")),
            ("Promise<Foo>", None),
            ("Optional<Foo>", None),
            ("Vec<Foo>", None),
            ("list[Foo]", None),
            ("Foo[]", None),
            ("Foo | Bar", None),
            ("ns::Foo", None),
            ("pkg.Foo", None),
            ("(Foo, error)", None),
            ("() => Foo", None),
            ("", None),
            ("null", None),
            ("void", None),
        ] {
            assert_eq!(return_type_key(raw), want, "return_type_key({raw:?})");
        }
    }

    /// Contract: a C# return type lookup walks out of the declaring file's
    /// namespace innermost first, by whole `.` segments, and stops before
    /// the global namespace, which level 0 already holds.
    #[test]
    fn test_enclosing_namespaces_innermost_first_by_segment() {
        for (namespace, want) in [
            ("Lib.Linq.Inner", vec!["Lib.Linq", "Lib"]),
            ("Lib.Linq", vec!["Lib"]),
            ("Lib", vec![]),
            ("", vec![]),
        ] {
            assert_eq!(
                enclosing_namespaces(namespace).collect::<Vec<_>>(),
                want,
                "{namespace:?}"
            );
        }
    }

    /// Contract: every type an annotation mentions is a candidate, per
    /// language, with the markers dropped and a qualified name kept whole.
    /// The wrappers stay in the list: the resolver binds only project types.
    #[test]
    fn test_annotation_type_names_per_language_lists_every_mentioned_type() {
        use Language::*;
        let rows: &[(Language, &str, &[&str])] = &[
            (Rust, "St", &["St"]),
            (Rust, "Option<St>", &["Option", "St"]),
            (
                Rust,
                "Result<Option<Projection>, DbError>",
                &["Result", "Option", "Projection", "DbError"],
            ),
            (Rust, "&'a mut St", &["St"]),
            (Rust, "&'static [St]", &["St"]),
            (Rust, "Vec<parse::Entry>", &["Vec", "parse::Entry"]),
            (Rust, "crate::t::St", &["crate::t::St"]),
            (Rust, "Box<dyn Handler + 'a>", &["Box", "Handler"]),
            (Rust, "impl Fn(St) -> Out", &["Fn", "St", "Out"]),
            (Rust, "Option<Self>", &["Option"]),
            (Rust, "[St; 3]", &["St"]),
            (TypeScript, "Promise<St>", &["Promise", "St"]),
            (TypeScript, "St[]", &["St"]),
            (TypeScript, "St | null", &["St"]),
            (TypeScript, "A | B | undefined", &["A", "B"]),
            (TypeScript, "ns.St", &["ns.St"]),
            (
                TypeScript,
                "Record<string, St[]>",
                &["Record", "string", "St"],
            ),
            (TypeScript, "'admin' | St", &["St"]),
            (TypeScript, "readonly St[]", &["St"]),
            (JavaScript, "Array<St>", &["Array", "St"]),
            (Python, "\"St\"", &["St"]),
            (Python, "Optional[St]", &["Optional", "St"]),
            (Python, "list[St] | None", &["list", "St"]),
            (Python, "dict[str, \"St\"]", &["dict", "str", "St"]),
            (Python, "'St' | None", &["St"]),
            (Python, "mod.St", &["mod.St"]),
            (Java, "List<St>", &["List", "St"]),
            (
                Java,
                "Map<String, List<St>>",
                &["Map", "String", "List", "St"],
            ),
            (Java, "? extends St", &["St"]),
            (Java, "java.util.List<St>", &["java.util.List", "St"]),
            (Kotlin, "List<St>?", &["List", "St"]),
            (Kotlin, "Map<String, St>", &["Map", "String", "St"]),
            (Kotlin, "(St) -> Unit", &["St", "Unit"]),
            (CSharp, "List<St>", &["List", "St"]),
            (CSharp, "St?", &["St"]),
            (
                CSharp,
                "Dictionary<string, St>[]",
                &["Dictionary", "string", "St"],
            ),
            (Go, "[]pkg.St", &["pkg.St"]),
            (Go, "*St", &["St"]),
            (Go, "map[string]*St", &["string", "St"]),
            (Go, "...St", &["St"]),
            (Go, "chan St", &["St"]),
            (Go, "func(a St) (Out, error)", &["St", "Out", "error"]),
            (Php, "?St", &["St"]),
            (Php, "\\App\\Models\\St", &["App\\Models\\St"]),
            (Php, "static", &[]),
            (Php, "St|null", &["St"]),
            (Php, "array<int, St>", &["array", "int", "St"]),
            (Ruby, "", &[]),
            (Swift, "[St]", &["St"]),
            (Swift, "[String: St]", &["String", "St"]),
            (Swift, "Optional<St>", &["Optional", "St"]),
            (Swift, "St?", &["St"]),
            (Swift, "@escaping (St) -> Void", &["St", "Void"]),
            (Swift, "some Proto", &["Proto"]),
            (C, "const struct St *", &["St"]),
            (C, "unsigned int", &["int"]),
            (C, "St *const *", &["St"]),
            (Cpp, "std::vector<ns::St>", &["std::vector", "ns::St"]),
            (Cpp, "const St&", &["St"]),
            (
                Cpp,
                "std::map<std::string, St*>",
                &["std::map", "std::string", "St"],
            ),
            (Cpp, "std::array<St, 3>", &["std::array", "St"]),
            (Dart, "List<St>", &["List", "St"]),
            (Dart, "Future<St?>", &["Future", "St"]),
            (
                Dart,
                "Map<String, List<St>>",
                &["Map", "String", "List", "St"],
            ),
            (Dart, "prefix.St", &["prefix.St"]),
        ];
        for (language, raw, want) in rows {
            assert_eq!(
                annotation_type_names(*language, raw),
                *want,
                "{language:?} {raw:?}"
            );
        }
    }

    /// Contract: an empty, whitespace-only or punctuation-only annotation
    /// names no type, and a lone separator never joins a name.
    #[test]
    fn test_annotation_type_names_empty_or_symbols_returns_nothing() {
        for raw in ["", "   ", "\t\n", "<>", "[]", "*&", "->", "..."] {
            assert!(
                annotation_type_names(Language::Rust, raw).is_empty(),
                "{raw:?}"
            );
            assert!(
                annotation_type_names(Language::Go, raw).is_empty(),
                "{raw:?}"
            );
        }
        assert_eq!(annotation_type_names(Language::Cpp, "St::"), ["St"]);
        assert_eq!(annotation_type_names(Language::Go, "St.[]"), ["St"]);
    }

    /// Contract: an unterminated quote or a lone apostrophe never panics
    /// and keeps the names after it.
    #[test]
    fn test_annotation_type_names_unterminated_quote_keeps_following_names() {
        assert_eq!(annotation_type_names(Language::TypeScript, "\"St"), ["St"]);
        assert_eq!(
            annotation_type_names(Language::Rust, "'"),
            Vec::<&str>::new()
        );
        assert_eq!(
            annotation_type_names(Language::Python, "Ünï<St>"),
            ["Ünï", "St"]
        );
    }

    /// Contract: the reduced key is a fixed point, so the index (which
    /// stores the key) and the overlay (which reduces the raw text at query
    /// time) read the same type.
    #[test]
    fn test_return_type_key_reduced_key_returns_itself() {
        for raw in ["Foo", "?Foo", "&mut Self", "struct Foo", "\"Foo | None\""] {
            let key = return_type_key(raw).unwrap();
            assert_eq!(return_type_key(key), Some(key), "{raw:?}");
        }
    }
}
