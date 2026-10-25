//! C# extension methods as call targets (codex ruling P-ext).
//!
//! An extension method is a `static` method of a top-level, non-generic
//! `static` class whose first parameter carries `this`
//! (`static Task<bool> ToAsync(this bool value)`). `r.M()` reaches it only
//! when the receiver's type has no applicable member `M`, so the member
//! lookup on a known receiver type runs first, and R1b keeps it from ever
//! skipping past that receiver to a same-name global. This module decides
//! the retry after that lookup failed. The index (`Resolver`) and the
//! session overlay each collect the declarations and the receiver's types
//! by their own storage, then both read the verdict from [`resolve`].
//!
//! Scope forms: the caller file's namespaces and their enclosing ones, its
//! `using N;` and `using static N.C;` directives (a namespace-level
//! `using` counts for the whole file), and `global using` written in the
//! caller's own file. A `global using` of another file and assembly
//! boundaries (`internal` across projects) are not modeled; the BlindSpot
//! hint says so.
//!
//! The candidate unit is the method declaration (codex ruling P-ext2).
//! The call's argument count, optional parameters, a `params` array and
//! named arguments decide which declarations apply. Argument types and C#
//! betterness are not modeled, so two declarations that both apply stay
//! ambiguous.

use crate::analyzer::outcome::Tier;
use crate::analyzer::reexport::ResolveTarget;
use crate::analyzer::type_ladder::{owned_member, MemberLookup, TypeFacts};
use crate::analyzer::types::{CallArguments, RawImport};
use crate::file_category::FileMeta;

/// `FunctionMeta::flags` bit 10: a C# extension method, as
/// [`resolve`] recognises one (module doc). Bits 6-8 hold the visibility
/// and bit 9 C++ `virtual`.
pub const EXTENSION_FLAG: u16 = 1 << 10;

/// `FunctionMeta::flags` bit 11, set with [`EXTENSION_FLAG`]: the `this`
/// parameter's type is a type parameter of the method (`this T value`),
/// which applies to any receiver.
pub const GENERIC_THIS_FLAG: u16 = 1 << 11;

/// `FunctionMeta::flags` bit 12 of a C# extension method: its parameter
/// list ends in a `params` array. `FunctionMeta::params` does not list that
/// parameter. Bits 12-15 stay zero on any other method.
pub const PARAMS_ARRAY_FLAG: u16 = 1 << 12;

/// `FunctionMeta::flags` bits 13-15 of a C# extension method: how many of the
/// parameters `FunctionMeta::params` lists take a default value (`int n =
/// 0`). C# puts them last. The count saturates at [`OPTIONAL_SATURATED`].
pub const OPTIONAL_SHIFT: u16 = 13;

/// The largest optional-parameter count bits 13-15 hold: it reads as "this
/// many or more".
pub const OPTIONAL_SATURATED: u16 = 0b111;

/// The flag bits that record a C# method's optional parameters.
pub fn optional_bits(count: usize) -> u16 {
    (count.min(usize::from(OPTIONAL_SATURATED)) as u16) << OPTIONAL_SHIFT
}

/// The confidence of an extension call edge chosen without proof that it
/// is the one applicable candidate. No other call tier yields this value,
/// so the index and the overlay name the edge's reason by it.
pub const HEURISTIC_CONFIDENCE: f32 = 0.5;

/// The `reason` of a call edge at [`HEURISTIC_CONFIDENCE`].
pub const HEURISTIC_REASON: &str = "call:extension-heuristic";

/// BlindSpot `kind` of a C# call whose receiver type owns no such member
/// and whose extension lookup did not resolve.
pub const BS_KIND_UNRESOLVED: &str = "cs-extension-unresolved";

/// The visibility code of `private` in `FunctionMeta::flags` bits 6-8.
const PRIVATE: u16 = 2;

/// One recognised extension declaration named as the call's member.
#[derive(Debug, Clone)]
pub struct ExtensionDecl<'a> {
    pub id: u32,
    /// The `this` parameter's type as written (`IEnumerable<TSource>`).
    pub this_type: &'a str,
    /// Its `FunctionMeta::flags`.
    pub flags: u16,
    /// The names of its parameters after `this`, in order. A `params`
    /// array is not among them ([`PARAMS_ARRAY_FLAG`]).
    pub params: Vec<&'a str>,
    /// The file that declares it.
    pub file: &'a str,
    /// The owner key of its static class.
    pub class: &'a str,
    /// The namespace its class is declared in (`.`-joined, `""` for the
    /// global one); `None` when the file's namespaces cannot tell.
    pub namespace: Option<&'a str>,
}

/// Where the retry reads the receiver's type.
#[derive(Debug, Clone, Copy)]
pub enum ReceiverSite<'a> {
    /// A receiver the parser typed by name (`this`, `base`, a typed local or
    /// parameter), written `Type.member`; its type is read in the caller's
    /// file.
    Named(&'a str),
    /// The value a call to this function returns (`Read().ToAsync()`); its
    /// type is read in the function's file, as the chained call reads it.
    Returned(u32),
}

/// The call site's side of the lookup.
#[derive(Debug, Clone, Copy)]
pub struct CallerScope<'a> {
    pub file: &'a str,
    /// The namespaces the caller's file declares.
    pub namespaces: &'a [&'a str],
    pub imports: &'a [RawImport],
    /// The owner key of the type that declares the calling member.
    pub owner: Option<&'a str>,
}

/// The receiver: its type's name and the project types that name denotes
/// at the call (none for a library type, several for the parts of one
/// partial type or for namesakes).
#[derive(Debug, Clone, Copy)]
pub struct Receiver<'a> {
    pub name: &'a str,
    pub types: &'a [u32],
}

/// Why no extension edge binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unresolved {
    /// No declaration of that name is in the caller's scope.
    OutOfScope,
    /// Declarations are in scope, but none is visible and applicable to
    /// the receiver's type.
    Inapplicable,
    /// Declarations accept the receiver, but none accepts the call's
    /// arguments.
    Arguments,
    /// This many declarations apply, and the lookup cannot tell them apart.
    Ambiguous(u32),
}

/// Whether a declaration accepts the call's arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Applies {
    No,
    /// The record is absent, or a condition it cannot check decides.
    Unknown,
    Yes,
}

/// The retry's answer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// No extension method of that name is declared: no extension lookup.
    NotExtension,
    /// A receiver type declares or inherits the member: an instance
    /// member wins over every extension.
    InstanceMember,
    Bound {
        id: u32,
        confidence: f32,
    },
    Unresolved(Unresolved),
}

/// How a declaration's `this` parameter accepts the receiver. The order
/// ranks the fits of one class's overloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    Incompatible,
    /// The receiver may convert through a base the index cannot see.
    Maybe,
    /// A type parameter or `object` accepts any receiver.
    Generic,
    /// The `this` type names a resolved supertype of the receiver.
    Ancestor,
    /// The `this` type is the receiver's type.
    Exact,
}

/// The retry of `member` on `receiver` among `decls`, the recognised
/// extension declarations of that name, for a call that passes
/// `arguments` (`None` when the entry records none). An edge binds only
/// for exactly one declaration in scope, visible, accepting the receiver
/// and not refused by the arguments. It binds at the owned-member tier's
/// confidence when that declaration is proven: the receiver is one
/// project type whose whole ancestry is resolved and owns no `member`, the
/// `this` type names that type or a resolved supertype, and the arguments
/// fit its parameters. Every other single candidate binds at
/// [`HEURISTIC_CONFIDENCE`]. Two candidates, overloads of one class
/// included, are ambiguous.
pub fn resolve<F: TypeFacts + ?Sized>(
    facts: &F,
    decls: &[ExtensionDecl<'_>],
    receiver: Receiver<'_>,
    member: &str,
    arguments: Option<CallArguments<'_>>,
    caller: &CallerScope<'_>,
) -> Verdict {
    if decls.is_empty() {
        return Verdict::NotExtension;
    }
    let mut ancestors: Vec<&str> = Vec::new();
    let mut closed = receiver.types.len() == 1;
    for &ty in receiver.types {
        let mut lineage = vec![ty];
        lineage.extend(facts.ancestors(ty));
        for &t in &lineage {
            let Some(name) = facts.name_of(t) else {
                closed = false;
                continue;
            };
            if owned_member(facts, t, name, member, ResolveTarget::Callable) != MemberLookup::Miss {
                return Verdict::InstanceMember;
            }
            if t != ty {
                ancestors.push(name);
            }
            closed &= facts
                .supertypes(t)
                .is_none_or(|(_, first_unresolved)| first_unresolved.is_none());
        }
    }
    let receiver_key = canonical(receiver.name);
    let mut in_scope = false;
    let mut accepts_receiver = false;
    // (declaration, its fit, whether the arguments apply), one per
    // declaration: overloads of one class are rivals like any other.
    let mut candidates: Vec<(u32, Fit, Applies)> = Vec::new();
    for decl in decls {
        if !scope_admits(decl, caller) {
            continue;
        }
        in_scope = true;
        if (decl.flags >> 6) & 0b111 == PRIVATE && caller.owner != Some(decl.class) {
            continue;
        }
        let fit = fit(facts, decl, receiver_key, &ancestors, closed, caller.file);
        if fit == Fit::Incompatible {
            continue;
        }
        accepts_receiver = true;
        let applies = applies(decl, arguments);
        if applies != Applies::No {
            candidates.push((decl.id, fit, applies));
        }
    }
    match candidates.as_slice() {
        [] if accepts_receiver => Verdict::Unresolved(Unresolved::Arguments),
        [] if in_scope => Verdict::Unresolved(Unresolved::Inapplicable),
        [] => Verdict::Unresolved(Unresolved::OutOfScope),
        &[(id, fit, applies)] => {
            let proven =
                closed && matches!(fit, Fit::Exact | Fit::Ancestor) && applies == Applies::Yes;
            Verdict::Bound {
                id,
                confidence: if proven {
                    Tier::TypeOwned.confidence()
                } else {
                    HEURISTIC_CONFIDENCE
                },
            }
        }
        several => Verdict::Unresolved(Unresolved::Ambiguous(several.len() as u32)),
    }
}

/// The BlindSpot hint of an unresolved retry: the receiver type, the
/// member and the reason.
pub fn blind_spot_hint(receiver: &str, member: &str, why: Unresolved) -> String {
    let reason = match why {
        Unresolved::OutOfScope => format!(
            "no extension `{member}` in the caller's namespaces or `using`s \
             (a `global using` of another file is not modeled)"
        ),
        Unresolved::Inapplicable => {
            format!("no extension `{member}` in scope accepts a `{receiver}` receiver")
        }
        Unresolved::Arguments => format!(
            "no extension `{member}` that accepts a `{receiver}` receiver \
             takes the call's arguments"
        ),
        Unresolved::Ambiguous(n) => format!(
            "{n} extension declarations `{member}` apply \
             (argument types and C# betterness are not modeled)"
        ),
    };
    format!("`{receiver}` has no member `{member}`; extension lookup unresolved: {reason}")
}

/// Whether `decl` accepts `arguments`, the arguments after the receiver.
/// The count must fill every parameter without a default value; a
/// `params` array takes any surplus; each named argument must name a
/// parameter no positional argument filled. A record that is absent, an
/// optional count past [`OPTIONAL_SATURATED`], or a name that only the
/// unlisted `params` array could carry decide nothing: `Unknown`.
fn applies(decl: &ExtensionDecl<'_>, arguments: Option<CallArguments<'_>>) -> Applies {
    let Some(arguments) = arguments else {
        return Applies::Unknown;
    };
    let count = decl.params.len();
    let params_array = decl.flags & PARAMS_ARRAY_FLAG != 0;
    let optional = (decl.flags >> OPTIONAL_SHIFT) & OPTIONAL_SATURATED;
    let positional = arguments.positional as usize;
    if positional > count && !params_array {
        return Applies::No;
    }
    let mut filled = vec![false; count];
    filled[..positional.min(count)].fill(true);
    let mut verdict = Applies::Yes;
    for name in arguments.named() {
        match decl.params.iter().position(|&param| param == name) {
            Some(at) if filled[at] => return Applies::No,
            Some(at) => filled[at] = true,
            None if params_array && positional <= count => verdict = Applies::Unknown,
            None => return Applies::No,
        }
    }
    let required = count.saturating_sub(usize::from(optional));
    if filled[..required].iter().any(|&set| !set) {
        // Past saturation, more parameters than the bits say may be
        // optional.
        return if optional == OPTIONAL_SATURATED {
            Applies::Unknown
        } else {
            Applies::No
        };
    }
    verdict
}

/// Whether the declaration's class is in the caller's scope (module doc).
fn scope_admits(decl: &ExtensionDecl<'_>, caller: &CallerScope<'_>) -> bool {
    let Some(namespace) = decl.namespace else {
        return false;
    };
    let encloses = |own: &str| {
        own == namespace
            || own
                .strip_prefix(namespace)
                .is_some_and(|rest| rest.starts_with('.'))
    };
    let names_class = |path: &str| match namespace {
        "" => path == decl.class,
        _ => {
            path.strip_prefix(namespace)
                .and_then(|rest| rest.strip_prefix('.'))
                == Some(decl.class)
        }
    };
    namespace.is_empty()
        || caller.namespaces.iter().any(|own| encloses(own))
        || caller.imports.iter().any(|import| {
            let path = import.source.as_str();
            let path = path.strip_prefix("global::").unwrap_or(path);
            import.alias.is_none() && (path == namespace || names_class(path))
        })
}

fn fit<F: TypeFacts + ?Sized>(
    facts: &F,
    decl: &ExtensionDecl<'_>,
    receiver: &str,
    ancestors: &[&str],
    closed: bool,
    caller_file: &str,
) -> Fit {
    if decl.flags & GENERIC_THIS_FLAG != 0 {
        return Fit::Generic;
    }
    let Some(written) = this_type_key(decl.this_type) else {
        return Fit::Incompatible;
    };
    let this = canonical(written);
    if this == "Object" {
        return Fit::Generic;
    }
    if this == receiver {
        return Fit::Exact;
    }
    if ancestors
        .iter()
        .any(|&ancestor| canonical(ancestor) == this)
    {
        return Fit::Ancestor;
    }
    if closed || (is_sealed_predefined(receiver) && !names_interface(this)) {
        return Fit::Incompatible;
    }
    // A base the index cannot see is a library type, and no library type
    // derives from a project type.
    let project_type = !facts
        .global_candidates(
            written,
            ResolveTarget::Type,
            FileMeta::from_normalized_path(caller_file),
        )
        .is_empty();
    if project_type {
        Fit::Incompatible
    } else {
        Fit::Maybe
    }
}

/// The type name a `this` parameter's type reduces to: nullable marker,
/// generic arguments and namespace path dropped (`IEnumerable<T>?` →
/// `IEnumerable`, `global::System.Type` → `Type`). `None` for an array,
/// tuple, pointer or function type, which no named receiver type matches.
fn this_type_key(raw: &str) -> Option<&str> {
    let ty = raw.trim().trim_end_matches('?').trim_end();
    if ty.ends_with(']') || ty.starts_with('(') || ty.contains('*') {
        return None;
    }
    let head = ty.split('<').next().unwrap_or(ty).trim();
    let key = head.rsplit(['.', ':']).next().unwrap_or(head).trim();
    (!key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(key)
}

/// A C# keyword type as its `System` type's name (`bool` → `Boolean`).
fn canonical(name: &str) -> &str {
    match name {
        "bool" => "Boolean",
        "byte" => "Byte",
        "sbyte" => "SByte",
        "char" => "Char",
        "decimal" => "Decimal",
        "double" => "Double",
        "float" => "Single",
        "int" => "Int32",
        "uint" => "UInt32",
        "nint" => "IntPtr",
        "nuint" => "UIntPtr",
        "long" => "Int64",
        "ulong" => "UInt64",
        "short" => "Int16",
        "ushort" => "UInt16",
        "object" => "Object",
        "string" => "String",
        other => other,
    }
}

/// A sealed `System` type a C# keyword names: it converts only to itself,
/// `object`, `ValueType` and the interfaces it implements.
fn is_sealed_predefined(canonical_name: &str) -> bool {
    matches!(
        canonical_name,
        "Boolean"
            | "Byte"
            | "SByte"
            | "Char"
            | "Decimal"
            | "Double"
            | "Single"
            | "Int32"
            | "UInt32"
            | "IntPtr"
            | "UIntPtr"
            | "Int64"
            | "UInt64"
            | "Int16"
            | "UInt16"
            | "String"
    )
}

/// The .NET naming convention of an interface (`IComparable`), or
/// `ValueType`, which a sealed keyword type may also convert to.
fn names_interface(name: &str) -> bool {
    let mut chars = name.chars();
    name == "ValueType"
        || (chars.next() == Some('I') && chars.next().is_some_and(char::is_uppercase))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_this_type_key_wrappers_return_bare_name_or_none() {
        for (raw, want) in [
            ("bool", Some("bool")),
            ("IEnumerable<TSource>", Some("IEnumerable")),
            ("IEnumerable<KeyValuePair<string, T>>?", Some("IEnumerable")),
            ("global::System.Type", Some("Type")),
            (" Foo? ", Some("Foo")),
            ("int[]", None),
            ("(int, string)", None),
            ("int*", None),
            ("", None),
            ("   ", None),
        ] {
            assert_eq!(this_type_key(raw), want, "this_type_key({raw:?})");
        }
    }

    #[test]
    fn test_canonical_keyword_and_system_name_meet() {
        assert_eq!(canonical("bool"), canonical("Boolean"));
        assert_eq!(canonical("string"), "String");
        assert_eq!(canonical("JToken"), "JToken");
    }

    #[test]
    fn test_names_interface_convention_only() {
        assert!(names_interface("IComparable"));
        assert!(names_interface("ValueType"));
        assert!(!names_interface("Type"));
        assert!(!names_interface("Item"));
        assert!(!names_interface("I"));
    }

    fn decl<'a>(namespace: Option<&'a str>, class: &'a str) -> ExtensionDecl<'a> {
        ExtensionDecl {
            id: 0,
            this_type: "Foo",
            flags: EXTENSION_FLAG,
            params: Vec::new(),
            file: "Ext.cs",
            class,
            namespace,
        }
    }

    fn using(source: &str, alias: Option<&str>) -> RawImport {
        RawImport {
            source: source.to_owned(),
            imported_name: source.to_owned(),
            alias: alias.map(str::to_owned),
            binding_kind: None,
        }
    }

    /// Contract: the caller's namespace and each enclosing one, a plain
    /// `using`, a `using static` of the class and the global namespace
    /// admit; a sibling or nested namespace, an alias `using` and an
    /// unknown namespace do not.
    #[test]
    fn test_scope_admits_namespace_forms() {
        let imports = [
            using("Lib.Ext", None),
            using("Other.Helpers.Ext2", None),
            using("Aliased", Some("A")),
        ];
        let caller = CallerScope {
            file: "App.cs",
            namespaces: &["App.Core"],
            imports: &imports,
            owner: None,
        };
        let admits = |ns: Option<&str>, class: &str| scope_admits(&decl(ns, class), &caller);
        assert!(admits(Some("App.Core"), "E"), "own namespace");
        assert!(admits(Some("App"), "E"), "enclosing namespace");
        assert!(admits(Some(""), "E"), "global namespace");
        assert!(admits(Some("Lib.Ext"), "E"), "using");
        assert!(admits(Some("Other.Helpers"), "Ext2"), "using static");
        assert!(!admits(Some("Other.Helpers"), "Ext3"), "another class");
        assert!(!admits(Some("App.Core.Inner"), "E"), "nested namespace");
        assert!(!admits(Some("Ap"), "E"), "a prefix that is no segment");
        assert!(!admits(Some("Aliased"), "E"), "alias using");
        assert!(!admits(None, "E"), "unknown namespace");
    }

    #[test]
    fn test_blind_spot_hint_names_receiver_member_and_reason() {
        let hint = blind_spot_hint("bool", "ToAsync", Unresolved::Ambiguous(2));
        assert!(hint.contains("`bool`"), "{hint}");
        assert!(hint.contains("`ToAsync`"), "{hint}");
        assert!(hint.contains("2 extension declarations"), "{hint}");
    }

    fn shaped(params: &'static [&'static str], flags: u16) -> ExtensionDecl<'static> {
        ExtensionDecl {
            params: params.to_vec(),
            flags: EXTENSION_FLAG | flags,
            ..decl(Some(""), "E")
        }
    }

    fn call(positional: u32, named: &[&str]) -> String {
        crate::analyzer::types::CallSite::with_arguments("Foo.M", positional, named)
    }

    fn applies_to(decl: &ExtensionDecl<'_>, raw: &str) -> Applies {
        applies(decl, crate::analyzer::types::CallSite::arguments(raw))
    }

    /// Contract (P-ext2): the count fills every parameter without a
    /// default; a surplus needs a `params` array; a named argument names a
    /// parameter no positional argument filled. An absent record and a
    /// saturated optional count prove nothing.
    #[test]
    fn test_applies_argument_shapes_decide_yes_no_or_unknown() {
        let two = shaped(&["a", "b"], 0);
        let optional = shaped(&["a", "b"], optional_bits(1));
        let params = shaped(&["a"], PARAMS_ARRAY_FLAG);
        let saturated = shaped(&["a", "b", "c", "d", "e", "f", "g", "h"], optional_bits(9));
        for (name, decl, raw, want) in [
            ("exact count", &two, call(2, &[]), Applies::Yes),
            ("too few", &two, call(1, &[]), Applies::No),
            ("too many", &two, call(3, &[]), Applies::No),
            ("zero for two", &two, call(0, &[]), Applies::No),
            ("named fills the rest", &two, call(1, &["b"]), Applies::Yes),
            ("named twice", &two, call(1, &["a"]), Applies::No),
            ("unknown name", &two, call(1, &["z"]), Applies::No),
            ("optional left out", &optional, call(1, &[]), Applies::Yes),
            ("optional given", &optional, call(2, &[]), Applies::Yes),
            ("required left out", &optional, call(0, &["b"]), Applies::No),
            ("params surplus", &params, call(4, &[]), Applies::Yes),
            ("params empty", &params, call(1, &[]), Applies::Yes),
            (
                "params by name",
                &params,
                call(1, &["rest"]),
                Applies::Unknown,
            ),
            (
                "saturated optional",
                &saturated,
                call(0, &[]),
                Applies::Unknown,
            ),
        ] {
            assert_eq!(applies_to(decl, &raw), want, "{name}");
        }
        assert_eq!(applies(&two, None), Applies::Unknown, "no record");
        assert_eq!(optional_bits(9), optional_bits(7), "saturates");
    }
}
