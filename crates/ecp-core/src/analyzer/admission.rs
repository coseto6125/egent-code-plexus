//! The candidate-admission gate of the name tiers (CONTRACTS §3): whether a
//! declaration may be what a call site names at all, decided before any
//! tier counts its candidates. A candidate the site cannot legally reach
//! neither decides uniqueness nor blocks a fallback. The index and the
//! session overlay both pass their candidates through [`Gate`], so the two
//! cannot drift.
//!
//! The rules run in order:
//!
//! 1. **Language.** The candidate's file is in a language the caller's code
//!    can reach: its own, or the other language of a pair that builds
//!    together ([`language_reaches`]). A Python import never names a
//!    TypeScript file, whatever the extension probe order.
//! 2. **Owner.** A bare `f()` in a language that reaches the enclosing
//!    type's members without a receiver ([`bare_call_reaches_members`])
//!    reaches no member of an unrelated type: only the caller's type, its
//!    enclosing types, the types nested in it and their supertypes.
//! 3. **Form.** An untyped member call `o.m()` reaches a method or a type's
//!    function, never a free function (Python: another module's function,
//!    since a module is a value), as [`member_call_admits`] states.
//!
//! The vendor barrier and the strict language rule of the global tier stay
//! in [`FileMeta::admits`]; C / C++ internal linkage stays in
//! [`crate::analyzer::c_family`].

use std::cell::OnceCell;

use crate::analyzer::ladder::{split_qualifier, CallPick};
use crate::analyzer::same_file_pick::{is_free, member_call_admits, PickFacts};
use crate::analyzer::types::owner_key;
use crate::file_category::{FileMeta, Language};
use crate::graph::NodeKind;

/// The shape of the site, as the gate reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SiteForm {
    Bare,
    Qualified,
    TypedMember,
    UntypedMember,
    UnprovenMember,
    Construct,
    Value,
}

impl SiteForm {
    /// The form of a name-tier lookup of `name`: no call site is a value
    /// (or type) lookup, an untyped receiver an untyped member call, a
    /// qualified name (`K.m`, `ns::f`, a typed `T.m`) a qualified one.
    pub fn of(name: &str, call: Option<CallPick<'_>>) -> Self {
        match call {
            None => Self::Value,
            Some(call) if call.untyped => Self::UntypedMember,
            Some(_) if split_qualifier(name).is_some() => Self::Qualified,
            Some(_) => Self::Bare,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Site<'c> {
    pub caller: FileMeta,
    pub caller_file: &'c str,
    /// Owner key of the calling member's type (`CallPick::owner`).
    pub caller_owner: Option<&'c str>,
    /// The caller type's declared bases (`CallContext::heritage`).
    pub heritage: &'c [String],
    pub form: SiteForm,
    /// The name the site looks up: a constructor of the type named so is a
    /// construction, not a member call.
    pub name: &'c str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Language,
    Owner,
    Form,
}

/// The languages whose sources build together, so one names the other's
/// declarations through an import or a qualifier: TypeScript and
/// JavaScript, Java and Kotlin, C and C++ (a `.h` is C++ by extension).
fn interop_family(language: Language) -> Option<u8> {
    match language {
        Language::TypeScript | Language::JavaScript => Some(0),
        Language::Java | Language::Kotlin => Some(1),
        Language::C | Language::Cpp => Some(2),
        _ => None,
    }
}

/// Whether code of `caller` may name a declaration of a file of `candidate`
/// at all: the same language, or a pair of [`interop_family`]. The vendor
/// flag plays no part: an import names a vendored module legally.
pub fn language_reaches(caller: FileMeta, candidate: FileMeta) -> bool {
    caller.language == candidate.language
        || interop_family(caller.language)
            .is_some_and(|family| interop_family(candidate.language) == Some(family))
}

/// Whether a call written with no receiver reaches a member of the
/// enclosing type in `language` (the complement of
/// [`crate::analyzer::types::CallSite::reaches_free_only`]'s languages).
pub fn bare_call_reaches_members(language: Language) -> bool {
    matches!(
        language,
        Language::Java
            | Language::Kotlin
            | Language::CSharp
            | Language::Swift
            | Language::Dart
            | Language::Cpp
            | Language::Ruby
    )
}

/// How deep the gate follows the lexical nesting of types. Deeper nesting
/// counts as unrelated, which can only drop an edge, never invent one.
const MAX_NESTING: usize = 8;

/// The gate of one site. The related types of the Owner rule are found on
/// the first candidate that needs them and kept for every later pass over
/// the candidates.
pub struct Gate<'f, F: PickFacts + ?Sized> {
    facts: &'f F,
    site: Site<'f>,
    owner_rule: bool,
    related: OnceCell<Vec<u32>>,
}

impl<'f, F: PickFacts + ?Sized> Gate<'f, F> {
    pub fn new(facts: &'f F, site: Site<'f>) -> Self {
        Self {
            facts,
            site,
            owner_rule: site.form == SiteForm::Bare
                && bare_call_reaches_members(site.caller.language),
            related: OnceCell::new(),
        }
    }

    /// Whether candidate `id`, declared in a file of `meta`, may be what the
    /// site names. Runs before any uniqueness decision.
    pub fn admit(&self, id: u32, meta: FileMeta) -> Result<(), Reject> {
        if !language_reaches(self.site.caller, meta) {
            return Err(Reject::Language);
        }
        self.admit_in_file(id)
    }

    /// [`Self::admit`] for a candidate of the caller's own file, whose
    /// language the caller always reaches.
    pub fn admit_in_file(&self, id: u32) -> Result<(), Reject> {
        if self.owner_rule && !self.owner_reachable(id) {
            return Err(Reject::Owner);
        }
        if matches!(
            self.site.form,
            SiteForm::UntypedMember | SiteForm::UnprovenMember
        ) && !self.member_form_admits(id)
        {
            return Err(Reject::Form);
        }
        Ok(())
    }

    /// The candidates [`Self::admit`] lets through, in their order.
    pub fn admitted<'g, I>(&'g self, candidates: I) -> impl Iterator<Item = (u32, FileMeta)> + 'g
    where
        I: Iterator<Item = (u32, FileMeta)> + 'g,
    {
        candidates.filter(move |&(id, meta)| self.admit(id, meta).is_ok())
    }

    /// R21 ahead of uniqueness: a method or a type's function, never a free
    /// function, a constructor or the caller's own type's member by name
    /// alone; in Python also another module's function.
    fn member_form_admits(&self, id: u32) -> bool {
        let facts = self.facts;
        member_call_admits(facts, id, self.site.caller_owner)
            || (self.site.caller.language == Language::Python
                && is_free(facts, id)
                && facts.file_of(id) != Some(self.site.caller_file))
    }

    /// Whether a bare call of the site reaches member `id`. Anything but a
    /// member of a project type passes: a free function, a namespace or
    /// module member, and a member whose owner names no project type (the
    /// file cannot tell a namespace from a library class).
    fn owner_reachable(&self, id: u32) -> bool {
        let facts = self.facts;
        let kind = facts.kind(id);
        if !matches!(
            kind,
            NodeKind::Method | NodeKind::Property | NodeKind::Constructor
        ) {
            return true;
        }
        let Some(owner) = facts.owner(id).and_then(owner_key) else {
            return true;
        };
        let site = self.site;
        if site.caller_owner.and_then(owner_key) == Some(owner)
            || (kind == NodeKind::Constructor && owner_key(site.name) == Some(owner))
            || site
                .heritage
                .iter()
                .any(|base| owner_key(base) == Some(owner))
        {
            return true;
        }
        if facts
            .file_of(id)
            .is_some_and(|file| facts.is_namespace_owner(file, owner))
        {
            return true;
        }
        let owner_types = facts.types_named(owner);
        if owner_types.is_empty() {
            return true;
        }
        let related = self.related();
        owner_types
            .iter()
            .any(|ty| related.contains(ty) || self.nested_in_caller(*ty))
    }

    /// The caller's types, the types enclosing them, and the supertypes of
    /// all of those: what a bare name inside the caller's type reaches.
    fn related(&self) -> &[u32] {
        self.related.get_or_init(|| {
            let facts = self.facts;
            let Some(owner) = self.site.caller_owner.and_then(owner_key) else {
                return Vec::new();
            };
            let mut types = facts.types_named(owner);
            let mut at = 0;
            let mut depth = 0;
            while at < types.len() && depth < MAX_NESTING {
                let level_end = types.len();
                for ty in types[at..level_end].to_vec() {
                    let Some(outer) = facts.owner(ty).and_then(owner_key) else {
                        continue;
                    };
                    for outer in facts.types_named(outer) {
                        if !types.contains(&outer) {
                            types.push(outer);
                        }
                    }
                }
                at = level_end;
                depth += 1;
            }
            for ty in types.clone() {
                for ancestor in facts.ancestors(ty) {
                    if !types.contains(&ancestor) {
                        types.push(ancestor);
                    }
                }
            }
            types
        })
    }

    /// Whether the type `ty` sits inside the caller's type: a Kotlin
    /// companion object, a nested class.
    fn nested_in_caller(&self, ty: u32) -> bool {
        let facts = self.facts;
        let Some(caller) = self.site.caller_owner.and_then(owner_key) else {
            return false;
        };
        let mut current = ty;
        for _ in 0..MAX_NESTING {
            let Some(outer) = facts.owner(current).and_then(owner_key) else {
                return false;
            };
            if outer == caller {
                return true;
            }
            match facts.types_named(outer).first() {
                Some(&next) => current = next,
                None => return false,
            }
        }
        false
    }
}

/// Whether candidate `id` (declared in a file of `meta`) may be what `site`
/// names. Runs before any uniqueness decision. A tier that reads several
/// candidates of one site keeps one [`Gate`] instead.
pub fn admit<F: PickFacts + ?Sized>(
    facts: &F,
    site: Site<'_>,
    id: u32,
    meta: FileMeta,
) -> Result<(), Reject> {
    Gate::new(facts, site).admit(id, meta)
}

/// The `candidates` [`admit`] lets through, in their order.
pub fn admitted<'a, F: PickFacts + ?Sized + 'a, I: Iterator<Item = (u32, FileMeta)> + 'a>(
    facts: &'a F,
    site: Site<'a>,
    candidates: I,
) -> impl Iterator<Item = (u32, FileMeta)> + 'a {
    let gate = Gate::new(facts, site);
    candidates.filter(move |&(id, meta)| gate.admit(id, meta).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::same_file_pick::ImplMember;
    use std::borrow::Cow;

    /// Declarations `(name, kind, owner, file)`, id = index.
    #[derive(Default)]
    struct Decls {
        decls: Vec<(&'static str, NodeKind, Option<&'static str>, &'static str)>,
        namespaces: Vec<&'static str>,
        /// `(type id, ancestors)`.
        ancestors: Vec<(u32, Vec<u32>)>,
    }

    impl PickFacts for Decls {
        fn pick_candidates(&self, file: &str, name: &str) -> Cow<'_, [u32]> {
            Cow::Owned(
                (0..self.decls.len() as u32)
                    .filter(|&id| {
                        let (n, _, _, f) = self.decls[id as usize];
                        n == name && f == file
                    })
                    .collect(),
            )
        }
        fn kind(&self, id: u32) -> NodeKind {
            self.decls[id as usize].1
        }
        fn owner(&self, id: u32) -> Option<&str> {
            self.decls[id as usize].2
        }
        fn impl_member(&self, _id: u32) -> Option<ImplMember<'_>> {
            None
        }
        fn is_namespace_owner(&self, _file: &str, owner: &str) -> bool {
            self.namespaces.contains(&owner)
        }
        fn file_of(&self, id: u32) -> Option<&str> {
            Some(self.decls[id as usize].3)
        }
        fn types_named(&self, owner: &str) -> Vec<u32> {
            (0..self.decls.len() as u32)
                .filter(|&id| self.decls[id as usize].0 == owner && self.kind(id).is_type())
                .collect()
        }
        fn is_lexically_scoped(&self, _id: u32) -> bool {
            false
        }
        fn ancestors(&self, ty: u32) -> Vec<u32> {
            self.ancestors
                .iter()
                .find(|(t, _)| *t == ty)
                .map_or_else(Vec::new, |(_, a)| a.clone())
        }
    }

    const M: NodeKind = NodeKind::Method;
    const C: NodeKind = NodeKind::Class;

    fn site<'c>(
        caller_file: &'c str,
        caller_owner: Option<&'c str>,
        heritage: &'c [String],
        form: SiteForm,
    ) -> Site<'c> {
        Site {
            caller: FileMeta::from_path(caller_file),
            caller_file,
            caller_owner,
            heritage,
            form,
            name: "m",
        }
    }

    fn meta(path: &str) -> FileMeta {
        FileMeta::from_path(path)
    }

    // ---- Language ----

    #[test]
    fn test_admit_python_caller_typescript_candidate_rejects_language() {
        let facts = Decls {
            decls: vec![
                ("m", M, Some("Cls"), "cls.ts"),
                ("m", M, Some("Cls"), "cls.py"),
            ],
            ..Decls::default()
        };
        let at = site("use.py", None, &[], SiteForm::Qualified);
        assert_eq!(admit(&facts, at, 0, meta("cls.ts")), Err(Reject::Language));
        assert_eq!(admit(&facts, at, 1, meta("cls.py")), Ok(()));
    }

    #[test]
    fn test_admit_interop_pairs_admit_language() {
        let facts = Decls {
            decls: vec![("m", NodeKind::Function, None, "x")],
            ..Decls::default()
        };
        let pairs = [
            ("a.ts", "b.js"),
            ("a.js", "b.ts"),
            ("A.kt", "B.java"),
            ("A.java", "B.kt"),
            ("a.c", "b.h"),
            ("a.cpp", "b.c"),
            ("a.py", "b.pyi"),
        ];
        for (caller, candidate) in pairs {
            let at = site(caller, None, &[], SiteForm::Value);
            assert_eq!(
                admit(&facts, at, 0, meta(candidate)),
                Ok(()),
                "{caller} -> {candidate}"
            );
        }
        let at = site("a.rs", None, &[], SiteForm::Value);
        assert_eq!(admit(&facts, at, 0, meta("b.c")), Err(Reject::Language));
    }

    #[test]
    fn test_admit_vendor_candidate_keeps_language_rule_only() {
        let facts = Decls {
            decls: vec![("m", NodeKind::Function, None, "vendor/x.py")],
            ..Decls::default()
        };
        let at = site("a.py", None, &[], SiteForm::Value);
        assert_eq!(admit(&facts, at, 0, meta("vendor/x.py")), Ok(()));
    }

    // ---- Owner ----

    fn widget() -> Decls {
        Decls {
            decls: vec![
                ("lib_init", NodeKind::Function, None, "lib.c"),
                ("W", NodeKind::Struct, None, "widget.cpp"),
                ("lib_init", M, Some("W"), "widget.cpp"),
            ],
            ..Decls::default()
        }
    }

    #[test]
    fn test_admit_cpp_bare_call_unrelated_class_method_rejects_owner() {
        let facts = widget();
        let at = site("main.cpp", None, &[], SiteForm::Bare);
        assert_eq!(admit(&facts, at, 2, meta("widget.cpp")), Err(Reject::Owner));
        assert_eq!(admit(&facts, at, 0, meta("lib.c")), Ok(()));
    }

    #[test]
    fn test_admit_cpp_bare_call_own_class_method_admits() {
        let facts = widget();
        let at = site("widget.cpp", Some("W"), &[], SiteForm::Bare);
        assert_eq!(admit(&facts, at, 2, meta("widget.cpp")), Ok(()));
    }

    #[test]
    fn test_admit_bare_call_qualified_or_free_only_language_skips_owner_rule() {
        let facts = widget();
        let qualified = site("main.cpp", None, &[], SiteForm::Qualified);
        assert_eq!(admit(&facts, qualified, 2, meta("widget.cpp")), Ok(()));
        let python = Decls {
            decls: vec![("K", C, None, "k.py"), ("m", M, Some("K"), "k.py")],
            ..Decls::default()
        };
        let bare = site("a.py", None, &[], SiteForm::Bare);
        assert_eq!(admit(&python, bare, 1, meta("k.py")), Ok(()));
    }

    #[test]
    fn test_admit_java_bare_call_ancestor_enclosing_nested_and_heritage_admit() {
        let facts = Decls {
            decls: vec![
                ("Base", C, None, "Base.java"),
                ("Outer", C, None, "Outer.java"),
                ("Inner", C, Some("Outer"), "Outer.java"),
                ("Leaf", C, None, "Leaf.java"),
                ("Companion", C, Some("Leaf"), "Leaf.java"),
                ("Unrelated", C, None, "U.java"),
                ("m", M, Some("Base"), "Base.java"),
                ("m", M, Some("Outer"), "Outer.java"),
                ("m", M, Some("Companion"), "Leaf.java"),
                ("m", M, Some("Unrelated"), "U.java"),
                ("Lib", C, None, "Lib.java"),
                ("m", M, Some("Lib"), "Lib.java"),
            ],
            ancestors: vec![(3, vec![0])],
            ..Decls::default()
        };
        let leaf = site("Leaf.java", Some("Leaf"), &[], SiteForm::Bare);
        assert_eq!(
            admit(&facts, leaf, 6, meta("Base.java")),
            Ok(()),
            "ancestor"
        );
        assert_eq!(admit(&facts, leaf, 8, meta("Leaf.java")), Ok(()), "nested");
        assert_eq!(
            admit(&facts, leaf, 9, meta("U.java")),
            Err(Reject::Owner),
            "unrelated"
        );
        let inner = site("Outer.java", Some("Inner"), &[], SiteForm::Bare);
        assert_eq!(
            admit(&facts, inner, 7, meta("Outer.java")),
            Ok(()),
            "enclosing"
        );
        let bases = ["Lib<T>".to_string()];
        let unresolved = site("Leaf.java", Some("Leaf"), &bases, SiteForm::Bare);
        assert_eq!(
            admit(&facts, unresolved, 11, meta("Lib.java")),
            Ok(()),
            "heritage"
        );
    }

    #[test]
    fn test_admit_bare_call_namespace_member_unknown_owner_or_construction_admits() {
        let facts = Decls {
            decls: vec![
                ("f", M, Some("ns"), "a.cpp"),
                ("f", M, Some("Missing"), "b.cpp"),
                ("Foo", C, None, "foo.kt"),
                ("Foo", NodeKind::Constructor, Some("Foo"), "foo.kt"),
            ],
            namespaces: vec!["ns"],
            ..Decls::default()
        };
        let at = site("main.cpp", None, &[], SiteForm::Bare);
        assert_eq!(admit(&facts, at, 0, meta("a.cpp")), Ok(()));
        assert_eq!(admit(&facts, at, 1, meta("b.cpp")), Ok(()));
        let construct = Site {
            name: "Foo",
            ..site("app.kt", None, &[], SiteForm::Bare)
        };
        assert_eq!(admit(&facts, construct, 3, meta("foo.kt")), Ok(()));
    }

    // ---- Form ----

    #[test]
    fn test_admit_untyped_member_free_function_rejects_form() {
        let facts = Decls {
            decls: vec![
                ("m", NodeKind::Function, None, "a.js"),
                ("K", C, None, "k.js"),
                ("m", M, Some("K"), "k.js"),
            ],
            ..Decls::default()
        };
        let at = site("use.js", None, &[], SiteForm::UntypedMember);
        assert_eq!(admit(&facts, at, 0, meta("a.js")), Err(Reject::Form));
        assert_eq!(admit(&facts, at, 2, meta("k.js")), Ok(()));
        let unproven = site("use.js", None, &[], SiteForm::UnprovenMember);
        assert_eq!(admit(&facts, unproven, 0, meta("a.js")), Err(Reject::Form));
    }

    #[test]
    fn test_admit_untyped_member_python_other_module_function_admits_own_module_rejects() {
        let facts = Decls {
            decls: vec![
                ("m", NodeKind::Function, None, "api.py"),
                ("m", NodeKind::Function, None, "use.py"),
            ],
            ..Decls::default()
        };
        let at = site("use.py", None, &[], SiteForm::UntypedMember);
        assert_eq!(admit(&facts, at, 0, meta("api.py")), Ok(()));
        assert_eq!(admit(&facts, at, 1, meta("use.py")), Err(Reject::Form));
    }

    #[test]
    fn test_admit_untyped_member_own_type_member_rejects_form() {
        let facts = Decls {
            decls: vec![("K", C, None, "k.js"), ("m", M, Some("K"), "k.js")],
            ..Decls::default()
        };
        let at = site("k.js", Some("K"), &[], SiteForm::UntypedMember);
        assert_eq!(admit(&facts, at, 1, meta("k.js")), Err(Reject::Form));
    }

    // ---- order and stream ----

    #[test]
    fn test_admit_rules_run_in_order_language_first() {
        let facts = Decls {
            decls: vec![("m", NodeKind::Function, None, "a.ts")],
            ..Decls::default()
        };
        let at = site("use.py", None, &[], SiteForm::UntypedMember);
        assert_eq!(admit(&facts, at, 0, meta("a.ts")), Err(Reject::Language));
    }

    #[test]
    fn test_admitted_empty_stream_yields_nothing() {
        let facts = Decls::default();
        let at = site("a.py", None, &[], SiteForm::Bare);
        assert_eq!(admitted(&facts, at, std::iter::empty()).count(), 0);
    }

    #[test]
    fn test_gate_admitted_read_twice_keeps_order_and_related_cache() {
        let facts = Decls {
            decls: vec![
                ("Leaf", C, None, "Leaf.java"),
                ("Other", C, None, "O.java"),
                ("m", M, Some("Leaf"), "Leaf.java"),
                ("m", M, Some("Other"), "O.java"),
                ("m", M, Some("Leaf"), "Leaf2.java"),
            ],
            ..Decls::default()
        };
        let gate = Gate::new(&facts, site("Leaf.java", Some("Leaf"), &[], SiteForm::Bare));
        let stream = || {
            [
                (2, meta("Leaf.java")),
                (3, meta("O.java")),
                (4, meta("Leaf2.java")),
            ]
        };
        let first: Vec<u32> = gate
            .admitted(stream().into_iter())
            .map(|(id, _)| id)
            .collect();
        let second: Vec<u32> = gate
            .admitted(stream().into_iter())
            .map(|(id, _)| id)
            .collect();
        assert_eq!(first, vec![2, 4]);
        assert_eq!(first, second);
    }

    #[test]
    fn test_site_form_of_call_shapes_returns_form() {
        let call = CallPick::default();
        let untyped = CallPick {
            untyped: true,
            ..call
        };
        assert_eq!(SiteForm::of("m", None), SiteForm::Value);
        assert_eq!(SiteForm::of("m", Some(untyped)), SiteForm::UntypedMember);
        assert_eq!(SiteForm::of("K.m", Some(call)), SiteForm::Qualified);
        assert_eq!(SiteForm::of("ns::f", Some(call)), SiteForm::Qualified);
        assert_eq!(SiteForm::of("m", Some(call)), SiteForm::Bare);
    }
}
