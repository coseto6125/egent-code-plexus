//! The C / C++ rule of the global (Tier 3) tier. The index
//! (`ecp-analyzer` `SymbolTable::c_family_field`) and the session overlay
//! (`session/view/resolution.rs`) both run [`c_family_field`] over their own
//! candidates, so a dirty file binds what a reindex binds.
//!
//! A `.h` file is C++ by extension (PR #142), so the language barrier of
//! [`FileMeta::admits`] keeps a C caller from every type and function its
//! headers declare. Two rules repair that without opening the barrier to
//! any other pair:
//!
//! - **Family fallback.** When the caller's own language leaves a prototype
//!   that none of its definitions settles, or offers no candidate at all, a
//!   C caller also weighs the C++ candidates and a C++ caller the C
//!   candidates that both languages name the same way: a top-level
//!   (owner-less) function, struct, enum, typedef, variable, constant or
//!   macro. A class, a method and anything inside a namespace or a type
//!   stay C++ only. A function of internal linkage (`static`, or in an
//!   anonymous namespace) never crosses, and a vendor one never crosses to a
//!   non-vendor caller. Beside
//!   an owner-less prototype, a crossing definition of another parameter
//!   list is another overload, not the declared function, so it stays out;
//!   so does an own-language definition when a crossing prototype of the
//!   name is settled by no definition (`f(void)` against `f(int)`).
//!   An own-language definition with no prototype anywhere
//!   always wins, so a name that a C file and a C++ file both define keeps
//!   its old binding.
//! - **Declaration and definition are one target.** A C / C++ function
//!   declared without a body (a header prototype: `FunctionMeta::FLAG_EXTERN`
//!   on a `Function`) leaves the field when a definition in it settles it:
//!   the same owner, the same parameter types, and a definition the caller
//!   can reach (no vendor one for a non-vendor caller, no internal-linkage
//!   one in another file). `f()` is the unspecified form of C, so a
//!   definition with parameters settles it. Two definitions stay ambiguous,
//!   and so does a prototype that no definition settles. A callable whose
//!   parse paired no meta, or whose meta lacks a parameter's type, has no
//!   known parameter types: it settles nothing.

use crate::analyzer::same_file_pick::PickFacts;
use crate::file_category::{FileMeta, Language};
use crate::graph::{FunctionMeta, NodeKind};
use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};

pub fn is_c_family(language: Language) -> bool {
    matches!(language, Language::C | Language::Cpp)
}

/// Whether a callable has a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CBody {
    Definition,
    Declaration,
}

/// Whether other files can link to a callable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CLinkage {
    External,
    Internal,
}

/// A callable's parameter list in the form its source wrote.
#[derive(Debug, Clone, Copy)]
pub enum CParams {
    /// `f(int)`, `f(void)`. `arity` counts a `...` as one parameter.
    Prototype {
        arity: u16,
        variadic: bool,
        signature: Option<u64>,
    },
    /// `f()`: unspecified in C and in a declaration a C definition settles;
    /// zero parameters between two C++ files.
    EmptyParens,
    /// The identifier list of a K&R definition.
    OldStyle { arity: u16, signature: Option<u64> },
}

impl CParams {
    fn signature(self) -> Option<u64> {
        match self {
            Self::Prototype { signature, .. } | Self::OldStyle { signature, .. } => signature,
            Self::EmptyParens => None,
        }
    }

    /// Whether the parameter types are known: `EmptyParens` has none to know.
    fn is_known(self) -> bool {
        matches!(self, Self::EmptyParens) || self.signature().is_some()
    }

    fn arity(self) -> u16 {
        match self {
            Self::Prototype { arity, .. } | Self::OldStyle { arity, .. } => arity,
            Self::EmptyParens => 0,
        }
    }
}

/// What the `FunctionMeta` of a C or C++ `Function` / `Method` tells the
/// rule. A callable whose parse paired no meta has none.
#[derive(Debug, Clone, Copy)]
pub struct CCallable {
    pub body: CBody,
    pub linkage: CLinkage,
    pub params: CParams,
}

impl CCallable {
    /// The facts of a callable whose meta carries `flags` and the flat
    /// `params` list `[name1, type1, name2, type2, ..]`.
    pub fn new<'s>(flags: u16, params: impl IntoIterator<Item = &'s str>) -> Self {
        let mut hasher = FxHasher::default();
        let mut known = true;
        let mut variadic = false;
        let mut name = "";
        let mut len = 0usize;
        for text in params {
            if len.is_multiple_of(2) {
                name = text;
            } else if !text.trim().is_empty() {
                hash_type(&mut hasher, text);
            } else if name == "..." {
                variadic = true;
                hash_type(&mut hasher, name);
            } else {
                known = false;
            }
            len += 1;
        }
        let arity = u16::try_from(len / 2).unwrap_or(u16::MAX);
        let signature = known.then(|| hasher.finish());
        let params = match flags & FunctionMeta::C_PARAM_FORM_MASK {
            FunctionMeta::C_PARAM_FORM_EMPTY_PARENS => CParams::EmptyParens,
            FunctionMeta::C_PARAM_FORM_OLD_STYLE => CParams::OldStyle { arity, signature },
            _ => CParams::Prototype {
                arity,
                variadic,
                signature,
            },
        };
        let internal =
            flags & (FunctionMeta::FLAG_STATIC | FunctionMeta::FLAG_C_INTERNAL_LINKAGE) != 0;
        Self {
            body: if flags & FunctionMeta::FLAG_EXTERN != 0 {
                CBody::Declaration
            } else {
                CBody::Definition
            },
            linkage: if internal {
                CLinkage::Internal
            } else {
                CLinkage::External
            },
            params,
        }
    }
}

/// Feed the parameter type `ty` to `hasher` in one spelling: whitespace
/// kept only between two identifier characters (`T *` == `T*`), and a
/// leading elaborated keyword dropped (`struct foo` == `foo`, also across a
/// line break: `struct\nfoo`), so a C header and a C++ definition of one
/// function agree.
fn hash_type(hasher: &mut FxHasher, ty: &str) {
    let ty = ty.trim();
    let ty = ["struct", "enum", "union", "class"]
        .iter()
        .find_map(|keyword| {
            ty.strip_prefix(keyword)
                .filter(|rest| rest.starts_with(char::is_whitespace))
        })
        .map_or(ty, str::trim_start);
    let (mut gap, mut prev_ident) = (false, false);
    for c in ty.chars() {
        if c.is_whitespace() {
            gap = true;
            continue;
        }
        let ident = c.is_alphanumeric() || c == '_';
        if gap && prev_ident && ident {
            ' '.hash(hasher);
        }
        c.hash(hasher);
        (gap, prev_ident) = (false, ident);
    }
    // Ends the parameter, so `(a, bc)` and `(ab, c)` differ.
    hasher.write_u8(0xff);
}

/// A prototype's or a definition's owner, form and file.
#[derive(Clone, Copy)]
struct Decl<'f> {
    owner: Option<&'f str>,
    callable: CCallable,
    language: Language,
    /// The file is a `.h`: a C++ file by extension that C code includes.
    header: bool,
}

/// A candidate with the meta the ladder admits it by and the language its
/// file has: a crossing candidate carries the caller's language in the meta.
type Entry = (u32, FileMeta, Language);

fn is_dot_h(file: Option<&str>) -> bool {
    file.is_some_and(|file| file.ends_with(".h"))
}

fn is_header_file(file: &str) -> bool {
    [".h", ".hh", ".hpp", ".hxx", ".inl"]
        .iter()
        .any(|extension| file.ends_with(extension))
}

fn declaration<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    callable: &impl Fn(u32) -> Option<CCallable>,
    id: u32,
    language: Language,
) -> Option<Decl<'f>> {
    Some(Decl {
        owner: facts.owner(id),
        callable: callable(id)?,
        language,
        header: is_dot_h(facts.file_of(id)),
    })
}

/// The prototype `id`; `None` when `id` is no prototype.
fn prototype<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    callable: &impl Fn(u32) -> Option<CCallable>,
    id: u32,
    language: Language,
) -> Option<Decl<'f>> {
    if facts.kind(id) != NodeKind::Function {
        return None;
    }
    declaration(facts, callable, id, language)
        .filter(|decl| decl.callable.body == CBody::Declaration)
}

/// The definition `id`, a function or method with a body. A C++
/// `void ns::f() {}` parses as a Method owned by `ns`, so either kind
/// settles `ns`'s `f`. `None` for a prototype, any other node, and a
/// callable without meta.
fn definition<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    callable: &impl Fn(u32) -> Option<CCallable>,
    id: u32,
    language: Language,
) -> Option<Decl<'f>> {
    let kind = facts.kind(id);
    if !matches!(kind, NodeKind::Function | NodeKind::Method) {
        return None;
    }
    declaration(facts, callable, id, language)
        .filter(|decl| kind == NodeKind::Method || decl.callable.body == CBody::Definition)
}

/// Whether `definition` may be the function `prototype` declares: the same
/// owner, and parameter lists that no known pair of types separates. `f()`
/// is unspecified in C, so it matches any definition when C code or a `.h`
/// is involved; between two C++ files it declares zero parameters.
fn may_declare(prototype: &Decl<'_>, definition: &Decl<'_>) -> bool {
    if prototype.owner != definition.owner {
        return false;
    }
    let unspecified =
        prototype.language == Language::C || definition.language == Language::C || prototype.header;
    match (prototype.callable.params, definition.callable.params) {
        (CParams::EmptyParens, _) if unspecified => true,
        (declared, defined) => {
            declared.arity() == defined.arity()
                && match (declared.signature(), defined.signature()) {
                    (Some(declared), Some(defined)) => declared == defined,
                    _ => true,
                }
        }
    }
}

/// Whether a caller in `caller_file` may reach `id`: an internal-linkage
/// function (a `static` free function, an anonymous-namespace member) only
/// from its own file, or when a header declares it. The vendor rule is the
/// caller's [`FileMeta::admits`].
fn reachable<F: PickFacts + ?Sized>(
    facts: &F,
    caller_file: &str,
    id: u32,
    callable: Option<CCallable>,
) -> bool {
    if !callable.is_some_and(|callable| callable.linkage == CLinkage::Internal)
        || facts.kind(id) != NodeKind::Function
    {
        return true;
    }
    facts
        .file_of(id)
        .is_some_and(|file| file == caller_file || is_header_file(file))
}

/// The definitions in `entries` that settle prototypes for a caller in
/// `caller_file`. A definition the caller cannot reach settles nothing, or
/// the prototype would leave and the barrier then drop the definition too.
/// Neither does one whose parameter types are unknown.
fn settled_definitions<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    caller: FileMeta,
    caller_file: &str,
    entries: &[Entry],
    callable: &impl Fn(u32) -> Option<CCallable>,
) -> Vec<Decl<'f>> {
    entries
        .iter()
        .filter(|&&(id, meta, _)| {
            caller.admits(meta) && reachable(facts, caller_file, id, callable(id))
        })
        .filter_map(|&(id, _, language)| definition(facts, callable, id, language))
        .filter(|decl| decl.callable.params.is_known())
        .collect()
}

fn is_settled(prototype: &Decl<'_>, settled: &[Decl<'_>]) -> bool {
    settled
        .iter()
        .any(|definition| may_declare(prototype, definition))
}

/// The Tier 3 field of a C or C++ caller in `caller_file`, when the family
/// rules change it; `None` when `candidates` already is the field: the
/// caller is no C / C++ file, nothing was out of the caller's reach, and
/// some candidate passes the barrier and none of those is a prototype.
/// `callable` reads [`CCallable`] for a C / C++ `Function` or `Method`. A
/// crossing candidate carries the caller's language in its meta, with its
/// own vendor flag, so the ladder's barrier admits it and the vendor barrier
/// still applies.
pub fn c_family_field<F, I>(
    facts: &F,
    caller: FileMeta,
    caller_file: &str,
    candidates: impl Fn() -> I,
    callable: impl Fn(u32) -> Option<CCallable>,
) -> Option<Vec<(u32, FileMeta)>>
where
    F: PickFacts + ?Sized,
    I: Iterator<Item = (u32, FileMeta)>,
{
    field_entries(facts, caller, caller_file, &candidates, &callable)
        .map(|field| field.into_iter().map(|(id, meta, _)| (id, meta)).collect())
}

fn crossing_prototypes<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    callable: &impl Fn(u32) -> Option<CCallable>,
    crossing: &[Entry],
) -> Vec<Decl<'f>> {
    crossing
        .iter()
        .filter_map(|&(id, _, language)| prototype(facts, callable, id, language))
        .collect()
}

fn field_entries<F, I>(
    facts: &F,
    caller: FileMeta,
    caller_file: &str,
    candidates: &impl Fn() -> I,
    callable: &impl Fn(u32) -> Option<CCallable>,
) -> Option<Vec<Entry>>
where
    F: PickFacts + ?Sized,
    I: Iterator<Item = (u32, FileMeta)>,
{
    if !is_c_family(caller.language) {
        return None;
    }
    let admitted = || candidates().filter(move |&(_, meta)| caller.admits(meta));
    let crossing_iter = || {
        candidates()
            .filter(move |&(id, meta)| {
                is_c_family(meta.language)
                    && meta.language != caller.language
                    && facts.owner(id).is_none()
                    && c_nameable(facts.kind(id))
                    && !callable(id).is_some_and(|c| c.linkage == CLinkage::Internal)
            })
            .map(move |(id, meta)| {
                (
                    id,
                    FileMeta {
                        is_vendor: meta.is_vendor,
                        ..caller
                    },
                    meta.language,
                )
            })
            // The vendor barrier before the prototypes: a vendor prototype
            // the caller cannot reach must not push a reachable definition
            // out as another overload.
            .filter(move |&(_, meta, _)| caller.admits(meta))
    };
    let cross_family = || -> Vec<Entry> { crossing_iter().collect() };
    // Common outcome (no prototype in the field, none crossing): decided by
    // two scans with no collected vector, so the `None` answer allocates
    // nothing. Any other outcome falls through to the full rule below.
    let (mut out_of_reach, mut reachable_any, mut has_prototype) = (false, false, false);
    for (id, meta) in admitted() {
        if reachable(facts, caller_file, id, callable(id)) {
            reachable_any = true;
            if prototype(facts, callable, id, meta.language).is_some() {
                has_prototype = true;
                break;
            }
        } else {
            out_of_reach = true;
        }
    }
    if reachable_any
        && !has_prototype
        && !crossing_iter()
            .any(|(id, _, language)| prototype(facts, callable, id, language).is_some())
    {
        return out_of_reach.then(|| {
            admitted()
                .filter(|&(id, _)| reachable(facts, caller_file, id, callable(id)))
                .map(|(id, meta)| (id, meta, meta.language))
                .collect()
        });
    }
    let mut out_of_reach = false;
    let mut field: Vec<Entry> = Vec::new();
    for (id, meta) in admitted() {
        if reachable(facts, caller_file, id, callable(id)) {
            field.push((id, meta, meta.language));
        } else {
            out_of_reach = true;
        }
    }
    let mut prototypes: Vec<Decl<'_>> = field
        .iter()
        .filter_map(|&(id, _, language)| prototype(facts, callable, id, language))
        .collect();
    // Own definitions with no prototype beside them win, unless a crossing
    // prototype declares the name and no definition settles it: then those
    // definitions are other functions of the same name (`f(void)` against
    // `f(int)`).
    let mut pre_crossing = None;
    if !field.is_empty() && prototypes.is_empty() {
        let crossing = cross_family();
        if crossing_prototypes(facts, callable, &crossing).is_empty() {
            return out_of_reach.then_some(field);
        }
        pre_crossing = Some(crossing);
    }
    let own = field.len();
    let shadowing = pre_crossing.is_some();
    let mut settled = settled_definitions(facts, caller, caller_file, &field, callable);
    if shadowing || field.is_empty() || !prototypes.iter().all(|p| is_settled(p, &settled)) {
        let crossing = pre_crossing.unwrap_or_else(cross_family);
        prototypes.extend(crossing_prototypes(facts, callable, &crossing));
        if shadowing && prototypes.iter().all(|p| is_settled(p, &settled)) {
            return Some(field);
        }
        // A crossing definition is owner-less, so only an owner-less
        // prototype declares it.
        let declared = prototypes.iter().any(|p| p.owner.is_none());
        field.extend(crossing.into_iter().filter(|&(id, _, language)| {
            !declared
                || definition(facts, callable, id, language)
                    .is_none_or(|def| prototypes.iter().any(|p| may_declare(p, &def)))
        }));
        settled.extend(settled_definitions(
            facts,
            caller,
            caller_file,
            &field[own..],
            callable,
        ));
        if shadowing {
            let (mut index, declares) = (0, |id, language| {
                definition(facts, callable, id, language)
                    .is_none_or(|def| prototypes.iter().any(|p| may_declare(p, &def)))
            });
            field.retain(|&(id, _, language)| {
                index += 1;
                index > own || facts.owner(id).is_some() || declares(id, language)
            });
        }
    }
    if !settled.is_empty() {
        field.retain(|&(id, _, language)| {
            prototype(facts, callable, id, language).is_none_or(|p| !is_settled(&p, &settled))
        });
    }
    Some(field)
}

/// A member a class body in a header declares: the parse marks no in-class
/// declaration bodiless (`FLAG_EXTERN` is a prototype's), so a header method
/// stands for one. Only an out-of-line definition in a source file settles
/// it (`void ns::Inst::cb_ns(int &x) {}`): a second body of one member is an
/// ODR violation, so such a pair is a declaration and its definition.
fn member_declaration<'f, F: PickFacts + ?Sized>(
    facts: &'f F,
    callable: &impl Fn(u32) -> Option<CCallable>,
    id: u32,
    language: Language,
) -> Option<Decl<'f>> {
    if facts.kind(id) != NodeKind::Method || !facts.file_of(id).is_some_and(is_header_file) {
        return None;
    }
    declaration(facts, callable, id, language)
}

/// Whether `id` may be a declaration [`canonical_declaration`] replaces: a
/// prototype, or a method a header declares. The cheap test a side runs
/// before it gathers the candidates.
pub fn may_be_declaration<F: PickFacts + ?Sized>(
    facts: &F,
    id: u32,
    callable: Option<CCallable>,
) -> bool {
    callable.is_some_and(|callable| match facts.kind(id) {
        NodeKind::Function => callable.body == CBody::Declaration,
        NodeKind::Method => facts.file_of(id).is_some_and(is_header_file),
        _ => false,
    })
}

/// The definition that settles the declaration `id` for this caller when
/// exactly one does; else `id`. A call and a function value then reach one
/// declaration. `id` is a prototype, which the Tier 3 field rules settle, or
/// a header's in-class member declaration ([`member_declaration`]), which
/// only a definition of the same owner outside every header settles.
pub fn canonical_declaration<F, I>(
    facts: &F,
    caller: FileMeta,
    caller_file: &str,
    id: u32,
    candidates: impl Fn() -> I,
    callable: impl Fn(u32) -> Option<CCallable>,
) -> u32
where
    F: PickFacts + ?Sized,
    I: Iterator<Item = (u32, FileMeta)>,
{
    if !is_c_family(caller.language) {
        return id;
    }
    let Some(language) = candidates()
        .find(|&(candidate, _)| candidate == id)
        .map(|(_, meta)| meta.language)
    else {
        return id;
    };
    if let Some(declared) = prototype(facts, &callable, id, language) {
        let Some(field) = field_entries(facts, caller, caller_file, &candidates, &callable) else {
            return id;
        };
        return only_or(
            id,
            field.into_iter().filter_map(|(candidate, _, language)| {
                definition(facts, &callable, candidate, language)
                    .filter(|def| may_declare(&declared, def))
                    .map(|_| candidate)
            }),
        );
    }
    let Some(declared) = member_declaration(facts, &callable, id, language) else {
        return id;
    };
    // The owner test first: it is a string compare, and most same-named
    // candidates belong to other types.
    only_or(
        id,
        candidates().filter_map(|(candidate, meta)| {
            (candidate != id
                && facts.owner(candidate) == declared.owner
                && caller.admits(meta)
                && !facts.file_of(candidate).is_some_and(is_header_file))
            .then(|| definition(facts, &callable, candidate, meta.language))
            .flatten()
            .filter(|def| may_declare(&declared, def))
            .map(|_| candidate)
        }),
    )
}

/// The one item of `settling`, else `id`.
fn only_or(id: u32, mut settling: impl Iterator<Item = u32>) -> u32 {
    match (settling.next(), settling.next()) {
        (Some(definition), None) => definition,
        _ => id,
    }
}

/// The kinds a C declaration and a C++ declaration share.
fn c_nameable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Typedef
            | NodeKind::Variable
            | NodeKind::Const
            | NodeKind::Macro
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ccallable_new_param_form_bits_select_the_form() {
        let prototype = CCallable::new(0, ["a", "int"]);
        let empty = CCallable::new(FunctionMeta::C_PARAM_FORM_EMPTY_PARENS, []);
        let old = CCallable::new(FunctionMeta::C_PARAM_FORM_OLD_STYLE, ["a", "int"]);
        assert!(matches!(
            prototype.params,
            CParams::Prototype {
                arity: 1,
                variadic: false,
                signature: Some(_)
            }
        ));
        assert!(matches!(empty.params, CParams::EmptyParens));
        assert!(matches!(
            old.params,
            CParams::OldStyle {
                arity: 1,
                signature: Some(_)
            }
        ));
    }

    #[test]
    fn test_ccallable_new_extern_storage_alone_is_a_definition() {
        let definition = CCallable::new(FunctionMeta::FLAG_C_EXTERN_STORAGE, []);
        let prototype = CCallable::new(FunctionMeta::FLAG_EXTERN, []);
        assert_eq!(definition.body, CBody::Definition);
        assert_eq!(prototype.body, CBody::Declaration);
    }

    #[test]
    fn test_ccallable_new_static_and_anonymous_namespace_are_internal() {
        assert_eq!(
            CCallable::new(FunctionMeta::FLAG_STATIC, []).linkage,
            CLinkage::Internal
        );
        assert_eq!(
            CCallable::new(FunctionMeta::FLAG_C_INTERNAL_LINKAGE, []).linkage,
            CLinkage::Internal
        );
        assert_eq!(CCallable::new(0, []).linkage, CLinkage::External);
    }

    #[test]
    fn test_ccallable_new_variadic_marker_sets_variadic_and_stays_known() {
        let callable = CCallable::new(0, ["a", "int", "...", ""]);
        assert!(matches!(
            callable.params,
            CParams::Prototype {
                arity: 2,
                variadic: true,
                signature: Some(_)
            }
        ));
    }

    #[test]
    fn test_ccallable_new_untyped_parameter_has_unknown_signature() {
        let callable = CCallable::new(0, ["a", " "]);
        assert!(!callable.params.is_known());
    }
}
