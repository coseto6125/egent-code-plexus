//! Resolution policy for a bare value or function reference.
//!
//! The index and the session overlay own different declaration stores. They
//! provide lookups as closures, while this module owns the policy so the two
//! paths cannot drift.

use crate::analyzer::reexport::ResolveTarget;
use crate::analyzer::same_file_pick::{prefer_free, PickFacts};
use crate::analyzer::type_ladder::{self, Ownership, TypeFacts};
use crate::analyzer::types::{owner_key, CallSite, NameRef, NameRefKind, RawImport};
use crate::file_category::Language;

#[derive(Clone, Copy)]
pub struct NameRefTiers {
    pub same_file: f32,
    pub heritage: f32,
    pub global: f32,
}

/// Resolve one [`NameRef`] through the index's ordinary symbol tiers.
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
    tiers: NameRefTiers,
    has_declaration: impl Fn(&str) -> bool,
    mut resolve: Resolve,
    mut resolve_imported: ResolveImported,
) -> Option<(u32, f32)>
where
    F: TypeFacts + ?Sized,
    Resolve: FnMut(&str, ResolveTarget) -> Vec<(u32, f32)>,
    ResolveImported: FnMut(&str, ResolveTarget) -> Vec<(u32, f32)>,
{
    let member = name_ref
        .name
        .rsplit_once('.')
        .map_or(name_ref.name, |(_, member)| member);
    if !has_declaration(member)
        && !imports
            .iter()
            .any(|import| import.alias.as_deref() == Some(member))
    {
        return None;
    }
    let qualified = member.len() != name_ref.name.len();
    let free_only = name_ref.kind == NameRefKind::Function
        && CallSite::Plain(name_ref.name).reaches_free_only(language);
    let target = match name_ref.kind {
        NameRefKind::Function => ResolveTarget::Callable,
        NameRefKind::Value | NameRefKind::Constant => ResolveTarget::Value,
    };
    if language == Language::Rust && !qualified {
        if let Some(id) = prefer_free(facts, file, member, owner, target) {
            return (name_ref.kind != NameRefKind::Constant || !facts.kind(id).is_callable())
                .then_some((id, tiers.same_file));
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
                return Some((*id, tiers.same_file));
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
                            tiers.same_file
                        } else {
                            tiers.heritage
                        };
                        return Some((id, confidence));
                    }
                }
            }
        }
    }
    let mut targets = resolve(name_ref.name, target);
    if targets.is_empty()
        && name_ref.kind == NameRefKind::Value
        && matches!(language, Language::Java | Language::Kotlin)
    {
        targets = resolve_imported(name_ref.name, ResolveTarget::Field);
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
    let global = confidence <= tiers.global;
    let other_package = facts.file_of(id).is_some_and(|target| {
        std::path::Path::new(target).parent() != std::path::Path::new(file).parent()
    });
    let type_member = name_ref.kind != NameRefKind::Function
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
                facts.count_in_file(target, owner, ResolveTarget::Callable) > 0
                    && facts.count_in_file(target, owner, ResolveTarget::Type) == 0
            })
        });
    if (module_scoped && global)
        || (language == Language::Go && global && other_package())
        || (name_ref.kind == NameRefKind::Constant && facts.kind(id).is_callable())
        || (name_ref.kind != NameRefKind::Function && type_member)
    {
        return None;
    }
    Some((id, confidence))
}
