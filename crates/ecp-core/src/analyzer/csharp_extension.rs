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
//! caller's own file. A `global using` of another file, assembly
//! boundaries (`internal` across projects) and the call's argument count
//! are not modeled; the BlindSpot hint says so.

use crate::analyzer::outcome::Tier;
use crate::analyzer::reexport::ResolveTarget;
use crate::analyzer::type_ladder::{owned_member, MemberLookup, TypeFacts};
use crate::analyzer::types::RawImport;
use crate::file_category::FileMeta;

/// `FunctionMeta::flags` bit 10: a C# extension method, as
/// [`resolve`] recognises one (module doc). Bits 6-8 hold the visibility
/// and bit 9 C++ `virtual`.
pub const EXTENSION_FLAG: u16 = 1 << 10;

/// `FunctionMeta::flags` bit 11, set with [`EXTENSION_FLAG`]: the `this`
/// parameter's type is a type parameter of the method (`this T value`),
/// which applies to any receiver.
pub const GENERIC_THIS_FLAG: u16 = 1 << 11;

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
#[derive(Debug, Clone, Copy)]
pub struct ExtensionDecl<'a> {
    pub id: u32,
    /// The `this` parameter's type as written (`IEnumerable<TSource>`).
    pub this_type: &'a str,
    /// Its `FunctionMeta::flags`.
    pub flags: u16,
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
    /// This many declaring classes apply, and the lookup cannot tell them
    /// apart.
    Ambiguous(u32),
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
/// extension declarations of that name. An edge binds only for exactly one
/// declaring class in scope, visible and applicable; its confidence is the
/// owned-member tier's when the receiver is one project type whose whole
/// ancestry is resolved and owns no `member`, and the `this` type names
/// that type or a resolved supertype. Every other single candidate binds
/// at [`HEURISTIC_CONFIDENCE`]. One class's overloads are one candidate,
/// bound at its first declaration: the call's arguments are not parsed.
pub fn resolve<F: TypeFacts + ?Sized>(
    facts: &F,
    decls: &[ExtensionDecl<'_>],
    receiver: Receiver<'_>,
    member: &str,
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
    // (file, class) → (its first declaration, its best fit)
    let mut classes: Vec<(&str, &str, u32, Fit)> = Vec::new();
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
        match classes
            .iter_mut()
            .find(|(file, class, ..)| *file == decl.file && *class == decl.class)
        {
            Some((_, _, first, best)) => {
                *first = (*first).min(decl.id);
                *best = (*best).max(fit);
            }
            None => classes.push((decl.file, decl.class, decl.id, fit)),
        }
    }
    match classes.as_slice() {
        [] if in_scope => Verdict::Unresolved(Unresolved::Inapplicable),
        [] => Verdict::Unresolved(Unresolved::OutOfScope),
        &[(_, _, id, fit)] => {
            let proven = closed && matches!(fit, Fit::Exact | Fit::Ancestor);
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
        Unresolved::Ambiguous(n) => format!(
            "{n} classes declare an applicable extension `{member}` \
             (call arguments and C# betterness are not modeled)"
        ),
    };
    format!("`{receiver}` has no member `{member}`; extension lookup unresolved: {reason}")
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
        assert!(hint.contains("2 classes"), "{hint}");
    }
}
