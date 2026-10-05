//! Import-member binding rules, shared by the index-time resolver
//! (ecp-analyzer `resolution/resolver.rs`) and the query-time overlay
//! (`session/import_scope.rs`), so the two cannot drift.
//!
//! Every function here is a pure function of the parse. Module discovery
//! (which file an import names) stays with each caller, because the index
//! reads its `SymbolTable` and the overlay reads the archived graph.

use crate::analyzer::types::RawImport;
use crate::file_category::Language;

/// Source extension of a language whose imports name a fully qualified class
/// or member path (`import a.b.C`, `use A\B\C`), not a file.
pub fn fqn_extension(language: Language) -> Option<&'static str> {
    match language {
        Language::Java => Some("java"),
        Language::Kotlin => Some("kt"),
        Language::Php => Some("php"),
        _ => None,
    }
}

/// Languages whose imports name a fully qualified class or member path.
pub fn fqn_language(language: Language) -> bool {
    fqn_extension(language).is_some()
}

/// Languages whose calls bind through an import's member: an import that
/// names the callee decides the target, or decides that there is none.
pub fn import_member_fallback(language: Language) -> bool {
    language == Language::Python || fqn_language(language)
}

/// The languages whose files a Missing import of `language` must also miss
/// before it may suppress. Java and Kotlin share packages, so a Java import
/// can name a Kotlin file and the reverse.
pub fn import_family(language: Language) -> impl Iterator<Item = Language> {
    let sibling = match language {
        Language::Java => Some(Language::Kotlin),
        Language::Kotlin => Some(Language::Java),
        _ => None,
    };
    std::iter::once(language).chain(sibling)
}

/// The owner a bound member must have inside the import's module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberOwner<'a> {
    /// The first declaration of the name in the module, in source order.
    Any,
    /// A declaration with no owning type: a package wildcard imports
    /// top-level functions and types, never the members of a class.
    TopLevel,
    /// A member of this type: `Helper.work()` through `import pkg.Helper`,
    /// or a static wildcard `import static pkg.Helper.*`.
    Type(&'a str),
}

/// What one import binds for one callee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportedMember<'a> {
    /// The member name to look up in the import's module.
    pub name: &'a str,
    pub owner: MemberOwner<'a>,
    /// A wildcard binds every bare name, so it ranks below the explicit
    /// imports and the caller's heritage.
    pub wildcard: bool,
}

/// The member a namespace import (`import pkg.util as u`) binds in `name`
/// (`u.helper` → `helper`).
pub fn namespace_member<'a>(import: &RawImport, name: &'a str) -> Option<&'a str> {
    if import.imported_name != "*" {
        return None;
    }
    name.strip_prefix(import.alias.as_deref()?)
        .and_then(|rest| rest.strip_prefix('.'))
        .filter(|member| !member.is_empty())
}

/// The last `.` / `\` segment of a fully qualified import path.
fn last_segment(path: &str) -> &str {
    path.rsplit(['.', '\\']).next().unwrap_or(path)
}

/// The module member `import` binds to the callee `name`, if it binds it.
pub fn import_binding<'a>(
    import: &'a RawImport,
    name: &'a str,
    language: Language,
) -> Option<ImportedMember<'a>> {
    if !fqn_language(language) {
        let member = if import.imported_name == "*" {
            namespace_member(import, name)?
        } else {
            (import.alias.as_deref().unwrap_or(&import.imported_name) == name)
                .then_some(import.imported_name.as_str())?
        };
        return Some(ImportedMember {
            name: member,
            owner: MemberOwner::Any,
            wildcard: false,
        });
    }
    let alias = import.alias.as_deref();
    if import.imported_name == "*" && matches!(alias, None | Some("*") | Some("static:*")) {
        if name.contains('.') {
            return None;
        }
        return Some(ImportedMember {
            name,
            owner: if alias == Some("static:*") {
                MemberOwner::Type(last_segment(&import.source))
            } else {
                MemberOwner::TopLevel
            },
            wildcard: true,
        });
    }
    // A PHP group member (`use A\{B as C}`) is a `*` import aliased to its
    // local name; its source names the member.
    let exported = if import.imported_name == "*" {
        last_segment(&import.source)
    } else {
        import.imported_name.as_str()
    };
    let binding = alias.unwrap_or(exported);
    if binding == name {
        return Some(ImportedMember {
            name: exported,
            owner: MemberOwner::Any,
            wildcard: false,
        });
    }
    let member = name.strip_prefix(binding)?.strip_prefix('.')?;
    (!member.is_empty()).then_some(ImportedMember {
        name: member,
        owner: MemberOwner::Type(exported),
        wildcard: false,
    })
}

/// The declared name a fully qualified import brings into scope under its
/// binding (`Logger` for `use Foo\Logger as Base`, `B` for `use A\{B}`).
pub fn fqn_imported_name(import: &RawImport) -> &str {
    if import.imported_name == "*" {
        last_segment(&import.source)
    } else {
        &import.imported_name
    }
}

/// The syntactic half of the suppression rule. A Missing import proves its
/// callee external only when it is absolute and, in a fully qualified
/// language, binds one name (no wildcard, no PHP group member) and names a
/// namespace. A single segment (`use Helper;`, a Kotlin default-package
/// import) can name an in-repo symbol whose file has another name.
pub fn may_suppress(import: &RawImport, language: Language) -> bool {
    !import.source.starts_with('.')
        && (!fqn_language(language)
            || (import.imported_name != "*"
                && import.source.trim_start_matches('\\').contains(['.', '\\'])))
}

/// The first segment of an import source. An indexed file stem or directory
/// of that name means the import may be local.
pub fn module_head(source: &str) -> &str {
    let source = source.trim_start_matches('\\');
    source.split(['/', '.', '\\']).next().unwrap_or(source)
}

/// The bare member a Kotlin call through an external class binding retries
/// instead of being suppressed: `ProtoBuf.asConverterFactory()` can reach an
/// in-repo extension function declared on the external type, which the
/// pre-import-tier resolver found by that bare name.
pub fn extension_retry<'a>(binding: &ImportedMember<'a>, language: Language) -> Option<&'a str> {
    (language == Language::Kotlin && matches!(binding.owner, MemberOwner::Type(_)))
        .then(|| binding.name.rsplit('.').next().unwrap_or(binding.name))
}

/// The name the general tiers retry when `import` binds `callee` but its
/// in-repo module lacks the member. Python retries the member name, without
/// imports; a fully qualified language keeps the callee as written (`None`),
/// so the call takes the resolution path it had before the import tier.
pub fn retry_name<'a>(
    import: &RawImport,
    member: &'a str,
    callee: &'a str,
    language: Language,
) -> Option<&'a str> {
    if fqn_language(language) {
        None
    } else if import.imported_name != "*" {
        Some(callee)
    } else {
        Some(member.rsplit('.').next().unwrap_or(member))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(source: &str, imported_name: &str, alias: Option<&str>) -> RawImport {
        RawImport {
            source: source.to_string(),
            imported_name: imported_name.to_string(),
            alias: alias.map(str::to_string),
            binding_kind: None,
        }
    }

    fn member(import: &RawImport, name: &str, language: Language) -> Option<String> {
        import_binding(import, name, language).map(|b| b.name.to_string())
    }

    #[test]
    fn test_import_binding_python_namespace_alias_binds_member() {
        let u = import("pkg/util", "*", Some("u"));
        assert_eq!(
            member(&u, "u.helper", Language::Python).as_deref(),
            Some("helper")
        );
        assert_eq!(member(&u, "helper", Language::Python), None);
        assert_eq!(member(&u, "u.", Language::Python), None);
    }

    #[test]
    fn test_import_binding_fqn_class_import_binds_member_of_class() {
        let class = import("pkg.Helper", "Helper", None);
        assert_eq!(
            import_binding(&class, "Helper.helper", Language::Java),
            Some(ImportedMember {
                name: "helper",
                owner: MemberOwner::Type("Helper"),
                wildcard: false,
            })
        );
        assert_eq!(member(&class, "Helper.", Language::Java), None);
    }

    #[test]
    fn test_import_binding_fqn_static_and_alias_bind_member() {
        let static_member = import("pkg.Helper.helper", "helper", Some("helper"));
        assert_eq!(
            member(&static_member, "helper", Language::Java).as_deref(),
            Some("helper")
        );
        let aliased = import("pkg.helper", "helper", Some("h"));
        assert_eq!(
            member(&aliased, "h", Language::Kotlin).as_deref(),
            Some("helper")
        );
        assert_eq!(member(&aliased, "helper", Language::Kotlin), None);
    }

    #[test]
    fn test_import_binding_wildcards_bind_bare_names_with_owner_rule() {
        let package = import("pkg", "*", None);
        assert_eq!(
            import_binding(&package, "helper", Language::Kotlin),
            Some(ImportedMember {
                name: "helper",
                owner: MemberOwner::TopLevel,
                wildcard: true,
            })
        );
        assert_eq!(member(&package, "a.helper", Language::Kotlin), None);
        let static_wildcard = import("pkg.Helper", "*", Some("static:*"));
        assert_eq!(
            import_binding(&static_wildcard, "helper", Language::Java).map(|b| b.owner),
            Some(MemberOwner::Type("Helper"))
        );
    }

    #[test]
    fn test_import_binding_php_group_member_binds_source_name() {
        let group = import("A\\B\\Logger", "*", Some("Base"));
        assert_eq!(
            import_binding(&group, "Base", Language::Php),
            Some(ImportedMember {
                name: "Logger",
                owner: MemberOwner::Any,
                wildcard: false,
            })
        );
        assert_eq!(fqn_imported_name(&group), "Logger");
    }

    #[test]
    fn test_may_suppress_single_segment_fqn_import_returns_false() {
        assert!(!may_suppress(
            &import("Helper", "Helper", None),
            Language::Php
        ));
        assert!(!may_suppress(
            &import("\\helper", "helper", None),
            Language::Php
        ));
        assert!(may_suppress(
            &import("External\\Helper", "Helper", None),
            Language::Php
        ));
        assert!(!may_suppress(&import("pkg", "*", None), Language::Kotlin));
        assert!(may_suppress(
            &import("external", "helper", None),
            Language::Python
        ));
        assert!(!may_suppress(
            &import(".util", "helper", None),
            Language::Python
        ));
    }

    #[test]
    fn test_module_head_fqn_and_path_sources_return_first_segment() {
        assert_eq!(module_head("\\App\\Models\\User"), "App");
        assert_eq!(module_head("com.app.Helper"), "com");
        assert_eq!(module_head("pkg/util"), "pkg");
    }

    #[test]
    fn test_import_family_java_and_kotlin_include_each_other() {
        assert_eq!(
            import_family(Language::Java).collect::<Vec<_>>(),
            [Language::Java, Language::Kotlin]
        );
        assert_eq!(
            import_family(Language::Php).collect::<Vec<_>>(),
            [Language::Php]
        );
    }
}
