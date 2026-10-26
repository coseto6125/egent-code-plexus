//! Path-alias expansion for module specifiers.
//!
//! Mirrors TypeScript's `compilerOptions.paths` semantics (a single `*`
//! wildcard per pattern, multiple replacement candidates per pattern,
//! tried in declaration order). The CLI parses `tsconfig.json` and
//! feeds the result into [`GraphBuilder::with_path_aliases`]; this
//! module knows nothing about JSON or filesystems — it only expands
//! string specifiers into candidate paths that the resolver's existing
//! file-key probing then matches against the `SymbolTable`.
//!
//! Example:
//!
//! ```
//! use ecp_core::analyzer::path_aliases::PathAliases;
//! let mut aliases = PathAliases::new();
//! aliases.add("@/*", vec!["src/*".to_string()]);
//! let mut got = vec![];
//! aliases.expand("@/components/Button", |c| {
//!     got.push(c.to_string());
//!     true
//! });
//! assert_eq!(got, vec!["src/components/Button".to_string()]);
//! ```

use crate::contracts::workspace::{DependencyTarget, Ecosystem, ScopeId, WorkspaceFacts};
use crate::file_category::Language;
use std::sync::Arc;

type Entries = Vec<(String, Vec<String>)>;

/// Ordered list of `(pattern, replacements)` entries.
///
/// Pattern syntax mirrors TypeScript: at most one `*` per pattern,
/// which captures everything between the literal prefix and suffix.
/// Patterns without `*` are exact-match only.
///
/// The entries are the repo-wide reading: the root configs. With
/// [`Self::with_scopes`] the table also holds one reading per C2 scope
/// (`contracts::workspace`), and [`Self::expand_in`] expands a specifier in
/// its importer's scope ([`Self::scope_of`]).
#[derive(Debug, Default, Clone)]
pub struct PathAliases {
    entries: Entries,
    /// `None` when the workspace has no scope of an ecosystem this reading
    /// serves: every importer then reads `entries`, at no lookup cost.
    scopes: Option<Arc<ScopeTables>>,
}

/// The C2 scope an importer resolves its imports in, for the alias reading
/// and the memo keys of module discovery. The default is no scope: the
/// repo-wide reading.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AliasScope(Option<ScopeId>);

#[derive(Debug)]
struct ScopeTables {
    facts: WorkspaceFacts,
    /// One per row of `facts.scopes`, by [`ScopeId`].
    tables: Vec<ScopeTable>,
}

/// The alias reading of one scope.
#[derive(Debug, Default)]
struct ScopeTable {
    /// The scope's effective aliases (`Scope::aliases`). Empty: the scope's
    /// config gives none (no `paths`, or a config that did not parse), and
    /// the repo-wide entries apply, as for a file in no scope.
    own: Entries,
    /// `name` → the entry files, `name/*` → the root, for each dependency
    /// that names a scope of the table (`DependencyTarget::Workspace`).
    /// After the aliases: an alias the config declares wins.
    dependencies: Entries,
}

impl ScopeTable {
    fn is_empty(&self) -> bool {
        self.own.is_empty() && self.dependencies.is_empty()
    }
}

/// The ecosystem whose scopes resolve the imports of a file in `language`,
/// for the ecosystems this reading serves. Rust keeps its crate table
/// (`RustWorkspaceModTree`, the stored Cargo rows) and the repo-wide
/// entries. Swift and every other language keep the repo-wide entries. The
/// invalidation closure reads it too, to find the files a dirty config
/// rescopes (`session::closure`).
pub(crate) fn import_ecosystem(language: Language) -> Option<Ecosystem> {
    match language {
        Language::TypeScript | Language::JavaScript => Some(Ecosystem::Node),
        Language::Go => Some(Ecosystem::Go),
        Language::Python => Some(Ecosystem::Python),
        Language::CSharp => Some(Ecosystem::DotNet),
        Language::Php => Some(Ecosystem::Composer),
        _ => None,
    }
}

fn join_root(root: &str, rest: &str) -> String {
    if root.is_empty() {
        rest.to_string()
    } else {
        format!("{root}/{rest}")
    }
}

impl PathAliases {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no reading holds an entry.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.scopes.is_none()
    }

    /// These entries as the repo-wide reading, plus one reading per scope of
    /// `facts` (R-A4b, C2 area 4). Built once per index or view: each scope's
    /// aliases, then an entry per workspace dependency. A table with no
    /// scope of a served ecosystem adds nothing.
    pub fn with_scopes(mut self, facts: &WorkspaceFacts) -> Self {
        let served = [
            Ecosystem::Node,
            Ecosystem::Go,
            Ecosystem::Python,
            Ecosystem::DotNet,
            Ecosystem::Composer,
        ];
        if !facts
            .scopes
            .iter()
            .any(|scope| served.contains(&scope.ecosystem))
        {
            self.scopes = None;
            return self;
        }
        let tables = facts
            .scopes
            .iter()
            .map(|scope| {
                let own = scope
                    .aliases
                    .iter()
                    .map(|alias| (alias.pattern.clone(), alias.replacements.clone()))
                    .collect();
                let mut dependencies = Entries::new();
                for dep in &scope.dependencies {
                    let DependencyTarget::Workspace(id) = dep.target else {
                        continue;
                    };
                    let Some(target) = facts.scope(id) else {
                        continue;
                    };
                    let entry = if target.entry_files.is_empty() {
                        (!target.root.is_empty()).then(|| vec![target.root.clone()])
                    } else {
                        Some(target.entry_files.clone())
                    };
                    if let Some(entry) = entry {
                        dependencies.push((dep.name.clone(), entry));
                    }
                    dependencies.push((
                        format!("{}/*", dep.name),
                        vec![join_root(&target.root, "*")],
                    ));
                }
                ScopeTable { own, dependencies }
            })
            .collect();
        self.scopes = Some(Arc::new(ScopeTables {
            facts: facts.clone(),
            tables,
        }));
        self
    }

    /// The scope `importer` (a repo-relative, `/`-separated path) resolves
    /// its imports in: the `Unique` C2 scope of its language's ecosystem.
    /// No scope for an `Ambiguous` or `Unscoped` file, for a language this
    /// reading does not serve, and when no scope table is attached. The
    /// lookup walks the importer's directories with one binary search each
    /// and allocates nothing.
    pub fn scope_of(&self, importer: &str) -> AliasScope {
        let Some(tables) = &self.scopes else {
            return AliasScope(None);
        };
        AliasScope(
            import_ecosystem(Language::from_normalized_path(importer))
                .and_then(|ecosystem| tables.facts.unique_scope_of(importer, ecosystem)),
        )
    }

    /// The root directory of `scope`; `None` for no scope. The repo root is
    /// the empty string.
    pub fn scope_root(&self, scope: AliasScope) -> Option<&str> {
        let tables = self.scopes.as_deref()?;
        Some(tables.facts.scope(scope.0?)?.root.as_str())
    }

    fn table(&self, scope: AliasScope) -> Option<&ScopeTable> {
        self.scopes.as_deref()?.tables.get(scope.0?.index())
    }

    /// Whether `scope` reads entries of its own: aliases its config
    /// declares, or a workspace dependency. `false` means the repo-wide
    /// reading, as for a file in no scope.
    pub fn has_scoped_table(&self, scope: AliasScope) -> bool {
        self.table(scope).is_some_and(|table| !table.is_empty())
    }

    /// [`Self::expand`] in `scope`: the scope's own aliases (the repo-wide
    /// entries when it has none), then its workspace dependencies. No
    /// scope reads the repo-wide entries alone.
    pub fn expand_in(
        &self,
        scope: AliasScope,
        specifier: &str,
        mut visit: impl FnMut(&str) -> bool,
    ) {
        let table = self.table(scope);
        let own = table
            .filter(|table| !table.own.is_empty())
            .map_or(&self.entries, |table| &table.own);
        if !expand_entries(own, specifier, &mut visit) {
            return;
        }
        if let Some(table) = table {
            expand_entries(&table.dependencies, specifier, &mut visit);
        }
    }

    /// [`Self::expand_in`] when `scope` has entries of its own
    /// ([`Self::has_scoped_table`]); nothing otherwise. For a reader that
    /// reads no repo-wide alias (the overlay's module discovery).
    pub fn expand_scoped(
        &self,
        scope: AliasScope,
        specifier: &str,
        visit: impl FnMut(&str) -> bool,
    ) {
        if self.has_scoped_table(scope) {
            self.expand_in(scope, specifier, visit);
        }
    }

    pub fn add(&mut self, pattern: impl Into<String>, replacements: Vec<String>) {
        self.entries.push((pattern.into(), replacements));
    }

    /// The `(pattern, replacements)` entries in declaration order, for
    /// serialising the table into `graph.bin`.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.entries
            .iter()
            .map(|(pattern, replacements)| (pattern.as_str(), replacements.as_slice()))
    }

    /// Walk every alias-derived candidate path for `specifier` in the
    /// repo-wide reading, invoking `visit` for each. Returning `false` from
    /// the closure short-circuits the remaining replacements + entries. An
    /// importer's scope reads [`Self::expand_in`].
    pub fn expand(&self, specifier: &str, mut visit: impl FnMut(&str) -> bool) {
        expand_entries(&self.entries, specifier, &mut visit);
    }
}

/// `false` when `visit` stopped the walk.
fn expand_entries(
    entries: &Entries,
    specifier: &str,
    visit: &mut impl FnMut(&str) -> bool,
) -> bool {
    for (pattern, replacements) in entries {
        let Some(matched) = match_pattern(pattern, specifier) else {
            continue;
        };
        for replacement in replacements {
            let expanded = apply_replacement(replacement, matched);
            if !visit(&expanded) {
                return false;
            }
        }
    }
    true
}

/// Match `specifier` against `pattern` (at most one `*`).
///
/// Returns the substring captured by the wildcard, or `Some("")` for
/// an exact (wildcard-free) match. `None` if no match.
fn match_pattern<'a>(pattern: &str, specifier: &'a str) -> Option<&'a str> {
    match pattern.split_once('*') {
        None => {
            if pattern == specifier {
                Some("")
            } else {
                None
            }
        }
        Some((prefix, suffix)) => {
            if specifier.len() < prefix.len() + suffix.len() {
                return None;
            }
            if !specifier.starts_with(prefix) || !specifier.ends_with(suffix) {
                return None;
            }
            Some(&specifier[prefix.len()..specifier.len() - suffix.len()])
        }
    }
}

/// Substitute `matched` for the `*` in `replacement`. If the replacement
/// has no `*`, return it verbatim (TypeScript treats this as a literal
/// fallback for the matched pattern).
fn apply_replacement(replacement: &str, matched: &str) -> String {
    match replacement.split_once('*') {
        None => replacement.to_string(),
        Some((prefix, suffix)) => {
            let mut out = String::with_capacity(prefix.len() + matched.len() + suffix.len());
            out.push_str(prefix);
            out.push_str(matched);
            out.push_str(suffix);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(aliases: &PathAliases, spec: &str) -> Vec<String> {
        let mut out = Vec::new();
        aliases.expand(spec, |c| {
            out.push(c.to_string());
            true
        });
        out
    }

    #[test]
    fn wildcard_prefix_expands() {
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into()]);
        assert_eq!(collect(&a, "@/utils"), vec!["src/utils"]);
        assert_eq!(
            collect(&a, "@/components/Button"),
            vec!["src/components/Button"]
        );
    }

    #[test]
    fn multiple_replacements_yield_multiple_candidates() {
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into(), "lib/*".into()]);
        assert_eq!(collect(&a, "@/x"), vec!["src/x", "lib/x"]);
    }

    #[test]
    fn exact_match_pattern_returns_replacement_verbatim() {
        let mut a = PathAliases::new();
        a.add("config", vec!["app/config/index.ts".into()]);
        assert_eq!(collect(&a, "config"), vec!["app/config/index.ts"]);
        assert_eq!(collect(&a, "config2"), Vec::<String>::new());
    }

    #[test]
    fn non_matching_specifier_yields_nothing() {
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into()]);
        assert_eq!(collect(&a, "./relative"), Vec::<String>::new());
        assert_eq!(collect(&a, "lodash"), Vec::<String>::new());
    }

    #[test]
    fn first_matching_entry_short_circuits_on_false() {
        // visitor returning false stops further expansion in the SAME entry
        // (matches the resolver convention used by for_each_specifier_candidate).
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into(), "lib/*".into()]);
        let mut out = Vec::new();
        a.expand("@/x", |c| {
            out.push(c.to_string());
            false
        });
        assert_eq!(out, vec!["src/x"]);
    }

    #[test]
    fn test_entries_in_declaration_order_with_duplicates_and_no_star() {
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into()]);
        a.add("config", vec!["app/config.ts".into()]);
        a.add("@/*", vec!["lib/*".into(), "vendor/*".into()]);
        let got: Vec<(&str, Vec<String>)> = a.entries().map(|(p, r)| (p, r.to_vec())).collect();
        assert_eq!(
            got,
            vec![
                ("@/*", vec!["src/*".to_string()]),
                ("config", vec!["app/config.ts".to_string()]),
                ("@/*", vec!["lib/*".to_string(), "vendor/*".to_string()]),
            ]
        );
    }

    #[test]
    fn test_entries_empty_table_yields_nothing() {
        assert_eq!(PathAliases::new().entries().count(), 0);
    }

    use crate::contracts::workspace::{
        ConfigPresence, ConfigRead, Membership, NameRank, Requirement, RequirementTarget, Scope,
        ScopeAlias, ScopeDraft,
    };

    /// One draft row: `manifest` makes the scope, `aliases` are its rows,
    /// `names` its primary names, `requires` its by-name dependencies.
    fn draft(
        ecosystem: Ecosystem,
        manifest: &str,
        aliases: &[(&str, &[&str])],
        names: &[&str],
        requires: &[&str],
    ) -> ScopeDraft {
        ScopeDraft {
            scope: Scope {
                ecosystem,
                root: manifest
                    .rsplit_once('/')
                    .map_or("", |(dir, _)| dir)
                    .to_string(),
                config_files: vec![ConfigRead {
                    path: manifest.to_string(),
                    presence: ConfigPresence::Read,
                }],
                membership: Membership::Subtree,
                entry_files: Vec::new(),
                aliases: aliases
                    .iter()
                    .map(|(pattern, replacements)| ScopeAlias {
                        pattern: pattern.to_string(),
                        replacements: replacements.iter().map(|r| r.to_string()).collect(),
                    })
                    .collect(),
                search_roots: Vec::new(),
                dependencies: Vec::new(),
            },
            names: names
                .iter()
                .map(|name| (name.to_string(), NameRank::Primary))
                .collect(),
            requires: requires
                .iter()
                .map(|name| Requirement {
                    name: name.to_string(),
                    target: RequirementTarget::ByName,
                })
                .collect(),
        }
    }

    fn repo_wide() -> PathAliases {
        let mut a = PathAliases::new();
        a.add("@/*", vec!["src/*".into()]);
        a
    }

    fn collect_in(aliases: &PathAliases, importer: &str, spec: &str) -> Vec<String> {
        let mut out = Vec::new();
        aliases.expand_in(aliases.scope_of(importer), spec, |c| {
            out.push(c.to_string());
            true
        });
        out
    }

    #[test]
    fn test_expand_in_two_packages_one_pattern_each_reads_own_package() {
        let facts = WorkspaceFacts::from_drafts(vec![
            draft(
                Ecosystem::Node,
                "packages/a/tsconfig.json",
                &[("@/*", &["packages/a/src/*"])],
                &[],
                &[],
            ),
            draft(
                Ecosystem::Node,
                "packages/b/tsconfig.json",
                &[("@/*", &["packages/b/src/*"])],
                &[],
                &[],
            ),
        ])
        .unwrap();
        let aliases = repo_wide().with_scopes(&facts);
        assert_eq!(
            collect_in(&aliases, "packages/a/main.ts", "@/util"),
            vec!["packages/a/src/util"]
        );
        assert_eq!(
            collect_in(&aliases, "packages/b/main.ts", "@/util"),
            vec!["packages/b/src/util"]
        );
        // The repo-wide reading stays for `expand` and for a file in no scope.
        assert_eq!(collect(&aliases, "@/util"), vec!["src/util"]);
        assert_eq!(
            collect_in(&aliases, "tools/x.ts", "@/util"),
            vec!["src/util"]
        );
    }

    #[test]
    fn test_expand_in_nested_scopes_innermost_wins() {
        let facts = WorkspaceFacts::from_drafts(vec![
            draft(
                Ecosystem::Node,
                "tsconfig.json",
                &[("@/*", &["src/*"])],
                &[],
                &[],
            ),
            draft(
                Ecosystem::Node,
                "apps/web/tsconfig.json",
                &[("@/*", &["apps/web/src/*"])],
                &[],
                &[],
            ),
        ])
        .unwrap();
        let aliases = PathAliases::new().with_scopes(&facts);
        assert_eq!(
            collect_in(&aliases, "apps/web/pages/a.ts", "@/x"),
            vec!["apps/web/src/x"]
        );
        assert_eq!(collect_in(&aliases, "lib/b.ts", "@/x"), vec!["src/x"]);
        assert_eq!(
            aliases.scope_root(aliases.scope_of("apps/web/a.ts")),
            Some("apps/web")
        );
        assert_eq!(aliases.scope_root(aliases.scope_of("lib/b.ts")), Some(""));
    }

    /// A scope whose config gave no alias (none declared, or a config that
    /// did not parse) reads the repo-wide entries, as a file in no scope.
    #[test]
    fn test_expand_in_scope_with_empty_alias_table_reads_repo_wide() {
        let facts = WorkspaceFacts::from_drafts(vec![draft(
            Ecosystem::Node,
            "packages/a/package.json",
            &[],
            &["a"],
            &[],
        )])
        .unwrap();
        let aliases = repo_wide().with_scopes(&facts);
        let scope = aliases.scope_of("packages/a/main.ts");
        assert_ne!(scope, AliasScope::default());
        assert!(!aliases.has_scoped_table(scope));
        assert_eq!(
            collect_in(&aliases, "packages/a/main.ts", "@/u"),
            vec!["src/u"]
        );
        let mut scoped = Vec::new();
        aliases.expand_scoped(scope, "@/u", |c| {
            scoped.push(c.to_string());
            true
        });
        assert!(scoped.is_empty());
    }

    /// A workspace dependency maps its name to the target's entry (its root
    /// when it lists none) and `name/*` under the root, after the aliases.
    #[test]
    fn test_expand_in_workspace_dependency_reads_target_root_after_aliases() {
        let mut b = draft(
            Ecosystem::Node,
            "packages/b/package.json",
            &[],
            &["@s/b"],
            &[],
        );
        b.scope.entry_files = vec!["packages/b/src/index.ts".to_string()];
        let facts = WorkspaceFacts::from_drafts(vec![
            draft(
                Ecosystem::Node,
                "packages/a/package.json",
                &[("#lib/*", &["packages/a/lib/*"])],
                &["@s/a"],
                &["@s/b", "left-pad"],
            ),
            b,
            draft(
                Ecosystem::Node,
                "packages/c/package.json",
                &[],
                &["@s/c"],
                &[],
            ),
        ])
        .unwrap();
        let aliases = PathAliases::new().with_scopes(&facts);
        let from_a = |spec: &str| collect_in(&aliases, "packages/a/x.ts", spec);
        assert_eq!(from_a("@s/b"), vec!["packages/b/src/index.ts"]);
        assert_eq!(from_a("@s/b/src/y"), vec!["packages/b/src/y"]);
        assert_eq!(from_a("#lib/z"), vec!["packages/a/lib/z"]);
        // An undeclared workspace package and an external one map nothing.
        assert!(from_a("@s/c").is_empty());
        assert!(from_a("left-pad").is_empty());
        assert!(aliases.has_scoped_table(aliases.scope_of("packages/a/x.ts")));
    }

    /// Two scopes at one root are ambiguous: no scope, the repo-wide
    /// entries. Rust, Swift and an unknown language keep them too.
    #[test]
    fn test_scope_of_ambiguous_or_unserved_language_returns_no_scope() {
        let facts = WorkspaceFacts::from_drafts(vec![
            draft(Ecosystem::DotNet, "svc/App.csproj", &[], &["App"], &[]),
            draft(Ecosystem::DotNet, "svc/Tests.csproj", &[], &["Tests"], &[]),
            draft(
                Ecosystem::Node,
                "tsconfig.json",
                &[("@/*", &["web/*"])],
                &[],
                &[],
            ),
            draft(
                Ecosystem::SwiftPm,
                "Package.swift",
                &[("Lib", &["Sources/Lib"])],
                &[],
                &[],
            ),
        ])
        .unwrap();
        let aliases = repo_wide().with_scopes(&facts);
        for importer in [
            "svc/Program.cs",
            "src/lib.rs",
            "Sources/Lib/a.swift",
            "README.md",
        ] {
            assert_eq!(
                aliases.scope_of(importer),
                AliasScope::default(),
                "{importer}"
            );
            assert_eq!(
                collect_in(&aliases, importer, "@/u"),
                vec!["src/u"],
                "{importer}"
            );
        }
        assert_eq!(collect_in(&aliases, "a.ts", "@/u"), vec!["web/u"]);
    }

    /// A table with no scope of a served ecosystem (a Cargo-only repo, an
    /// empty table) attaches nothing: no lookup runs.
    #[test]
    fn test_with_scopes_cargo_only_or_empty_attaches_nothing() {
        let cargo = WorkspaceFacts::from_drafts(vec![draft(
            Ecosystem::Cargo,
            "Cargo.toml",
            &[],
            &["ecp"],
            &[],
        )])
        .unwrap();
        for facts in [cargo, WorkspaceFacts::default()] {
            let aliases = PathAliases::new().with_scopes(&facts);
            assert!(aliases.is_empty());
            assert_eq!(aliases.scope_of("src/a.ts"), AliasScope::default());
        }
    }

    /// A C# project scope declares no alias, so today's repo-wide
    /// `ProjectReference` row (`Shared` → `Shared/*`) still expands first;
    /// the reference's workspace dependency follows it.
    #[test]
    fn test_expand_in_dotnet_project_keeps_repo_wide_project_reference_alias() {
        let mut repo = PathAliases::new();
        repo.add("Shared", vec!["Shared/*".into()]);
        let facts = WorkspaceFacts::from_drafts(vec![
            draft(Ecosystem::DotNet, "App.csproj", &[], &["App"], &["Shared"]),
            draft(
                Ecosystem::DotNet,
                "Shared/Shared.csproj",
                &[],
                &["Shared"],
                &[],
            ),
        ])
        .unwrap();
        let aliases = repo.with_scopes(&facts);
        assert_eq!(
            collect_in(&aliases, "Program.cs", "Shared"),
            vec!["Shared/", "Shared"]
        );
        assert_eq!(collect(&aliases, "Shared"), vec!["Shared/"]);
    }

    #[test]
    fn test_expand_in_visitor_false_stops_before_dependencies() {
        let facts = WorkspaceFacts::from_drafts(vec![draft(
            Ecosystem::Go,
            "go.mod",
            &[("ex.com/m/*", &["./*"])],
            &["ex.com/m"],
            &["ex.com/m"],
        )])
        .unwrap();
        let aliases = PathAliases::new().with_scopes(&facts);
        let mut out = Vec::new();
        aliases.expand_in(aliases.scope_of("cmd/main.go"), "ex.com/m/pkg", |c| {
            out.push(c.to_string());
            false
        });
        assert_eq!(out, vec!["./pkg"]);
    }

    #[test]
    fn suffix_in_pattern_is_respected() {
        let mut a = PathAliases::new();
        a.add("*.svg", vec!["assets/*.svg".into()]);
        assert_eq!(collect(&a, "logo.svg"), vec!["assets/logo.svg"]);
        assert_eq!(collect(&a, "logo.png"), Vec::<String>::new());
    }
}
