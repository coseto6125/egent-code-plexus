//! Resolution policy for a bare value or function reference.
//!
//! The index and the session overlay own different declaration stores. They
//! provide lookups as closures, while this module owns the policy so the two
//! paths cannot drift. Both reach it through [`crate::analyzer::ladder::name_ref`].

use crate::analyzer::import_binding;
use crate::analyzer::ladder::ResolveFacts;
use crate::analyzer::outcome::Tier;
use crate::analyzer::reexport::ResolveTarget;
use crate::analyzer::same_file_pick::prefer_free;
use crate::analyzer::type_ladder::{self, Ownership};
use crate::analyzer::types::{owner_key, CallSite, NameRef, NameRefKind, RawImport};
use crate::file_category::Language;
use crate::graph::NodeKind;

/// Resolve one [`NameRef`] through the index's ordinary symbol tiers, at
/// the [`Tier`] confidences.
///
/// `resolve` and `resolve_imported` apply the host's import and global
/// tables. The remaining policy is shared because it depends only on the
/// stable facts traits.
#[allow(clippy::too_many_arguments)]
pub fn resolve_name_ref<F, Resolve, ResolveImported>(
    facts: &F,
    file: &str,
    language: Language,
    imports: &[RawImport],
    owner: Option<&str>,
    name_ref: NameRef<'_>,
    mut resolve: Resolve,
    mut resolve_imported: ResolveImported,
) -> Option<(u32, f32)>
where
    F: ResolveFacts,
    Resolve: FnMut(&str, ResolveTarget) -> Vec<(u32, f32)>,
    ResolveImported: FnMut(&str, ResolveTarget) -> Vec<(u32, f32)>,
{
    // Rust `Self::parse` names the impl owner's function.
    let self_path;
    let full = match name_ref.name.strip_prefix("Self::") {
        Some(rest) if language == Language::Rust => {
            self_path = format!("{}::{rest}", owner.and_then(owner_key)?);
            self_path.as_str()
        }
        _ => name_ref.name,
    };
    // `A.Helpers.Format`: the tiers resolve `Helpers.Format`, then `A` must
    // be the path that encloses the type they bound. An import prefix (Dart
    // `p.Cls.f` after `import 'cls.dart' as p`) encloses nothing: the tiers
    // read the import for `Cls` alone.
    let (outer, name) = match full.rsplit_once('.') {
        Some((qualifier, _)) => match qualifier.rsplit_once('.') {
            Some((outer, _)) => (Some(outer), &full[outer.len() + 1..]),
            None => (None, full),
        },
        None => (None, full),
    };
    let outer = outer.filter(|outer| {
        !imports
            .iter()
            .any(|import| import.alias.as_deref() == Some(outer))
    });
    let member = name
        .rsplit_once('.')
        .or_else(|| {
            (language == Language::Rust)
                .then(|| name.rsplit_once("::"))
                .flatten()
        })
        .map_or(name, |(_, member)| member);
    // A namespace import (`import * as ns`) may reach `ns.renamed` through
    // an export alias that declares no `renamed`.
    if !facts.has_named(member)
        && !imports.iter().any(|import| {
            import.alias.as_deref() == Some(member)
                || import_binding::namespace_member(import, name).is_some()
        })
    {
        return None;
    }
    let qualified = member.len() != name.len();
    let free_only =
        name_ref.kind == NameRefKind::Function && CallSite::Plain(name).reaches_free_only(language);
    let target = match name_ref.kind {
        NameRefKind::Function => ResolveTarget::Callable,
        NameRefKind::Value | NameRefKind::Constant => ResolveTarget::Value,
    };
    if language == Language::Rust && !qualified {
        if let Some(id) = prefer_free(facts, file, member, owner, target) {
            return (name_ref.kind != NameRefKind::Constant || !facts.kind(id).is_callable())
                .then_some((id, Tier::SameFile.confidence()));
        }
    }
    if let (false, Some(owner)) = (qualified, owner) {
        let own_kinds: &[ResolveTarget] = match (name_ref.kind, language) {
            _ if free_only => &[],
            (NameRefKind::Function, _) => &[ResolveTarget::Method],
            (NameRefKind::Value, Language::CSharp | Language::Swift | Language::Dart) => {
                &[ResolveTarget::Field, ResolveTarget::Method]
            }
            (NameRefKind::Value, Language::Java | Language::Kotlin | Language::Cpp) => {
                &[ResolveTarget::Field]
            }
            _ => &[],
        };
        for &kind in own_kinds {
            if let [id] = facts.owned_in_file(file, member, owner, kind).as_slice() {
                return Some((*id, Tier::SameFile.confidence()));
            }
        }
        if let Some(key) = owner_key(owner).filter(|key| {
            !own_kinds.is_empty() && facts.count_in_file(file, key, ResolveTarget::Type) == 1
        }) {
            let type_id = facts
                .pick_candidates(file, key)
                .iter()
                .copied()
                .find(|&id| facts.kind(id).is_type());
            if let Some(type_id) = type_id {
                for &kind in own_kinds {
                    if let Ownership::Owned { id, inherited } =
                        type_ladder::member_ownership(facts, type_id, owner, member, kind)
                    {
                        let confidence = if !inherited && facts.file_of(id) == Some(file) {
                            Tier::SameFile.confidence()
                        } else {
                            Tier::HeritageScoped.confidence()
                        };
                        return Some((id, confidence));
                    }
                }
            }
        }
    }
    let mut targets = resolve(name, target);
    if targets.is_empty()
        && name_ref.kind == NameRefKind::Value
        && matches!(language, Language::Java | Language::Kotlin)
    {
        targets = resolve_imported(name, ResolveTarget::Field);
    }
    // A Python type alias (`IntOrStr = int | str`, a `Typedef`) is also a
    // value: `isinstance(x, IntOrStr)` reads it. Only a lookup that binds
    // the alias alone counts, so a class of the same name never binds. A
    // Global-tier value is no answer for Python (rejected below), so an
    // unrelated `Num = 3` in another file must not block the retry.
    if targets
        .iter()
        .all(|&(_, confidence)| confidence <= Tier::Global.confidence())
        && name_ref.kind != NameRefKind::Function
        && language == Language::Python
    {
        let aliases = resolve(name, ResolveTarget::Type);
        if let [(id, _)] = aliases.as_slice() {
            if facts.kind(*id) == NodeKind::Typedef {
                targets = aliases;
            }
        }
    }
    if free_only {
        targets.retain(|&(id, _)| facts.kind(id) != crate::graph::NodeKind::Method);
    }
    let &[(id, confidence)] = targets.as_slice() else {
        return None;
    };
    let module_scoped = matches!(
        language,
        Language::TypeScript
            | Language::JavaScript
            | Language::Python
            | Language::Java
            | Language::CSharp
    );
    let global = confidence <= Tier::Global.confidence();
    let other_package = facts.file_of(id).is_some_and(|target| {
        std::path::Path::new(target).parent() != std::path::Path::new(file).parent()
    });
    // PHP records a verified `Type.CONST` for a class or enum constant.
    // It must retain its member owner, while every other non-function value
    // keeps the existing type-member rejection.
    let php_qualified_constant =
        qualified && language == Language::Php && name_ref.kind == NameRefKind::Constant;
    let php_invisible_namespace_constant = language == Language::Php
        && global
        && name_ref.kind == NameRefKind::Constant
        && facts
            .file_of(id)
            .is_some_and(|target| !facts.namespace_visible_from(file, target));
    let type_member = !php_qualified_constant
        && name_ref.kind != NameRefKind::Function
        && !matches!(
            language,
            Language::Java
                | Language::Kotlin
                | Language::CSharp
                | Language::Swift
                | Language::Dart
                | Language::C
                | Language::Cpp
                | Language::Ruby
        )
        && facts.owner(id).is_some_and(|owner| {
            !facts.file_of(id).is_some_and(|target| {
                facts.is_namespace_owner(target, owner)
                    || (facts.count_in_file(target, owner, ResolveTarget::Callable) > 0
                        && facts.count_in_file(target, owner, ResolveTarget::Type) == 0)
            })
        });
    if (module_scoped && global)
        || (language == Language::Go && global && other_package)
        || php_invisible_namespace_constant
        || (name_ref.kind == NameRefKind::Constant && facts.kind(id).is_callable())
        || (name_ref.kind != NameRefKind::Function && type_member)
        || (outer.is_some()
            && !full
                .rsplit_once('.')
                .is_some_and(|(path, _)| encloses(facts, id, path)))
    {
        return None;
    }
    Some((id, confidence))
}

/// Whether the qualifier path `qualifier` (`A.Helpers`, `A.Deep.Outer.Inner`)
/// is the chain of types and namespaces that encloses `id`: from `id` out,
/// each owner key names the path's next segment, and the one declaration of
/// that name (`Outer`, or a dotted namespace `A.Deep`, `b::detail`) in
/// `id`'s file is the next link. Past the outermost owner, the rest of the
/// path must be a namespace or package the file declares (Java
/// `com.example`). A missing or a repeated link encloses nothing:
/// `System.Console.WriteLine` never binds a project `Console` that no
/// `System` holds.
fn encloses<F: ResolveFacts>(facts: &F, id: u32, qualifier: &str) -> bool {
    let Some(file) = facts.file_of(id) else {
        return false;
    };
    let segments: Vec<&str> = qualifier.split('.').collect();
    let mut end = segments.len();
    let mut node = id;
    while end > 0 {
        match facts.owner(node) {
            Some(owner) if owner == segments[end - 1] => {}
            // A Java or Kotlin package holds its top-level types without
            // owning them: the rest of the path is the package the file
            // declares.
            None => {
                let package = segments[..end].join(".");
                return facts.pick_candidates(file, &package).iter().any(|&link| {
                    matches!(facts.kind(link), NodeKind::Namespace | NodeKind::Module)
                });
            }
            Some(_) => return false,
        }
        let mut links = (0..end).flat_map(|start| {
            let path = &segments[start..end];
            let mut names = vec![path.join(".")];
            if path.len() > 1 {
                names.push(path.join("::"));
            }
            names.into_iter().flat_map(move |name| {
                facts
                    .pick_candidates(file, &name)
                    .iter()
                    .copied()
                    .filter(|&link| {
                        let kind = facts.kind(link);
                        kind.is_type() || matches!(kind, NodeKind::Namespace | NodeKind::Module)
                    })
                    .map(|link| (start, link))
                    .collect::<Vec<_>>()
            })
        });
        let (Some((start, link)), None) = (links.next(), links.next()) else {
            return false;
        };
        node = link;
        end = start;
    }
    true
}
