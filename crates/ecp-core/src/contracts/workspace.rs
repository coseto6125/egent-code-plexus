//! Contract C2: workspace facts (`docs/specs/contracts/C2-workspace.md`).
//!
//! One table of resolution scopes per indexed commit: the scope of each
//! file, the root of that scope, its effective configuration and its
//! dependency map. The index builds it once and stores it in `graph.bin`;
//! the index resolver, the overlay and the post-process `Imports` pass read
//! the same rows, and `ecp flow` runs the same producer. Every path in this
//! module is repo-relative, `/`-separated, with no leading `./` and no
//! trailing `/`; the repo root directory is the empty string.
//!
//! The table is small (one row per scope), so a reader deserializes it once
//! per view and queries the owned form. Rows carry plain `String`s, not
//! `StrRef`s, because the overlay builds rows for a dirty config file
//! without the graph's string pool.

use crate::analyzer::path_aliases::PathAliases;
use crate::contracts::routing::Language;
use rkyv::{Archive, Deserialize, Serialize};

/// The BlindSpot kind a consumer records for a file whose
/// [`WorkspaceFacts::scope_of`] answer is [`ScopeMatch::Ambiguous`]. The
/// consumer then resolves that file with no scope's config, never with one
/// picked from the candidates.
pub const SCOPE_AMBIGUOUS_BLIND_SPOT: &str = "workspace-scope-ambiguous";

/// The package ecosystem whose config files define a scope. Variants are
/// append-only: the discriminant is stored in `graph.bin`. A new variant
/// changes no layout, but an older binary rejects it, so it ships with a
/// `BUILDER_FINGERPRINT` bump.
#[derive(
    Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum Ecosystem {
    /// `Cargo.toml` packages and workspaces.
    Cargo,
    /// `package.json`, `tsconfig.json`, `jsconfig.json`, jest and webpack
    /// configs.
    Node,
    /// `go.mod` modules and `go.work` workspaces.
    Go,
    /// `pyproject.toml` projects.
    Python,
    /// `*.csproj` projects.
    DotNet,
}

/// What a config file name does to the scope layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigRole {
    /// The file can make its directory a scope root.
    Root,
    /// The file only adds to the effective config of the scopes that read
    /// it; it never makes a scope root.
    Contributing,
}

impl Ecosystem {
    /// The ecosystem whose scopes resolve the imports of a file in
    /// `language`; `None` for a language with no workspace facts yet. The
    /// caller passes C5's language after parse (`ParsedLanguage::resolution`),
    /// so a component with a script arrives as TypeScript or JavaScript.
    /// The component arms answer `Node` too, so a caller that passes the
    /// file's language gets the same scope. The match has no wildcard arm:
    /// a new `Language` variant fails to compile until it is mapped here.
    pub fn for_language(language: Language) -> Option<Self> {
        match language {
            Language::Rust => Some(Self::Cargo),
            Language::TypeScript
            | Language::JavaScript
            | Language::Vue
            | Language::Astro
            | Language::Svelte => Some(Self::Node),
            Language::Go => Some(Self::Go),
            Language::Python => Some(Self::Python),
            Language::CSharp => Some(Self::DotNet),
            Language::Java
            | Language::Kotlin
            | Language::Php
            | Language::Ruby
            | Language::Swift
            | Language::C
            | Language::Cpp
            | Language::Dart
            | Language::Bash
            | Language::Lua
            | Language::Crystal
            | Language::Move
            | Language::Solidity
            | Language::Dockerfile
            | Language::Nim
            | Language::Hcl
            | Language::Sql
            | Language::Vyper
            | Language::Verilog
            | Language::Cairo
            | Language::Zig
            | Language::Protobuf
            | Language::DockerCompose
            | Language::GitHubActions
            | Language::Kubernetes
            | Language::OpenApi
            | Language::Yaml => None,
        }
    }

    /// The ecosystem and role of a config file, by its file name alone.
    /// Being a config says nothing about being source: `webpack.config.ts`
    /// is both, so a change to it is parsed and also invalidates config.
    /// A file that only an `extends` (or similar) reference makes a config,
    /// such as `tsconfig.base.json`, is not named here: the table lists it
    /// in [`Scope::config_files`] instead, see
    /// [`WorkspaceFacts::config_ecosystem`].
    pub fn classify_config(file_name: &str) -> Option<(Self, ConfigRole)> {
        match file_name {
            "Cargo.toml" => Some((Self::Cargo, ConfigRole::Root)),
            "package.json" | "tsconfig.json" | "jsconfig.json" => {
                Some((Self::Node, ConfigRole::Root))
            }
            "jest.config.json" | "jest.config.js" | "jest.config.ts" | "webpack.config.js"
            | "webpack.config.ts" => Some((Self::Node, ConfigRole::Contributing)),
            "go.mod" => Some((Self::Go, ConfigRole::Root)),
            "go.work" => Some((Self::Go, ConfigRole::Contributing)),
            "pyproject.toml" => Some((Self::Python, ConfigRole::Root)),
            name if name.len() > ".csproj".len() && name.ends_with(".csproj") => {
                Some((Self::DotNet, ConfigRole::Root))
            }
            _ => None,
        }
    }
}

/// Index of a row in [`WorkspaceFacts::scopes`]. Valid only for the table
/// that issued it.
#[derive(
    Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[rkyv(derive(Debug))]
pub struct ScopeId(pub u32);

impl ScopeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Whether the producer found a config path it tried to read.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum ConfigPresence {
    /// The producer read the file's bytes.
    Read,
    /// The producer looked for the file and it did not exist: an `extends`
    /// target that is not there yet. Its later appearance changes the
    /// effective config, so the path stays a dependency.
    Missing,
}

/// One config path the producer tried to read for a scope.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct ConfigRead {
    pub path: String,
    pub presence: ConfigPresence,
}

/// Which files under the scope root belong to the scope.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum Membership {
    /// Every file of the ecosystem under `root`, unless a deeper root of the
    /// same ecosystem admits it first.
    Subtree,
    /// Exactly these files, strictly sorted, each under `root`: a
    /// `tsconfig.json` `files` / `include` set or a `*.csproj` compile list,
    /// expanded by the producer.
    Listed(Vec<String>),
}

impl Membership {
    /// Whether a file under the scope root belongs to the scope.
    pub fn admits(&self, path: &str) -> bool {
        match self {
            Self::Subtree => true,
            Self::Listed(files) => files
                .binary_search_by(|file| file.as_str().cmp(path))
                .is_ok(),
        }
    }
}

/// One resolution scope: one project manifest, the files it governs, one
/// effective config and one dependency map.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct Scope {
    pub ecosystem: Ecosystem,
    /// The scope root directory: the directory of the manifest.
    pub root: String,
    /// Every config path the effective config tried to read. The first is
    /// the manifest that made this scope: present, directly in `root`, and
    /// a `Root` config name of `ecosystem`. Its path is the scope's
    /// identity: `(ecosystem, root, manifest)` is unique, so `App.csproj`
    /// and `Tests.csproj` in one directory are two scopes. Then each
    /// inherited, extended or missing path (a workspace `Cargo.toml`, a
    /// `go.work`, an `extends` target, an ancestor `tsconfig.json`). A change
    /// to any of them, an appearance included, makes this scope stale.
    pub config_files: Vec<ConfigRead>,
    /// The files this scope governs.
    pub membership: Membership,
    /// The files a path through this scope's name starts from, best first.
    /// Cargo: the lib entry (`[lib] path`, else `src/lib.rs`, else
    /// `src/main.rs`), then the bin roots. May be empty.
    pub entry_files: Vec<String>,
    /// The effective path aliases in declaration order, the order the
    /// resolver tries them. Replacements are repo-relative, like today's
    /// `PathAliases` rows: the producer joins the scope root in front of a
    /// scope-relative replacement, and keeps a root scope's replacement
    /// verbatim. Each entry has at least one replacement.
    pub aliases: Vec<ScopeAlias>,
    /// Directories searched, in order, for a specifier that is neither
    /// relative nor aliased: a Python source root, a TS `baseUrl`, an
    /// include directory. Empty when the ecosystem searches none.
    pub search_roots: Vec<String>,
    /// The dependency map, sorted by `name`, names unique.
    pub dependencies: Vec<Dependency>,
}

/// One `(pattern, replacements)` entry of a scope's alias table, with the
/// pattern syntax of [`PathAliases`].
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct ScopeAlias {
    pub pattern: String,
    pub replacements: Vec<String>,
}

/// One declared dependency of a scope.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct Dependency {
    /// The name code writes for the dependency: a Cargo rename key, an npm
    /// package name, a Go module path.
    pub name: String,
    pub target: DependencyTarget,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum DependencyTarget {
    /// A scope of this table, of the same ecosystem.
    Workspace(ScopeId),
    /// A declared dependency outside the repo: a path under this name must
    /// not bind to a same-name project file.
    External,
}

/// Precedence of a name in [`WorkspaceFacts::names`].
#[derive(
    Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[rkyv(derive(Debug))]
#[repr(u8)]
pub enum NameRank {
    /// The name code writes: a Cargo `[lib] name` (else the package name
    /// with `-` replaced by `_`), an npm package name, a Go module path.
    Primary,
    /// Another accepted spelling, such as the raw Cargo package name. It
    /// answers only when no `Primary` entry has the name.
    Alternate,
}

/// One name by which code in another scope refers to `scope`.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct ScopeName {
    /// Equal to the ecosystem of `scope`; kept here so the sorted list
    /// answers a lookup without reading the scope rows.
    pub ecosystem: Ecosystem,
    pub name: String,
    pub rank: NameRank,
    pub scope: ScopeId,
}

/// The workspace facts of one commit. B's first round stores only Cargo
/// rows; area 4 adds rows of other ecosystems without a layout change.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[rkyv(derive(Debug))]
pub struct WorkspaceFacts {
    /// Sorted by `(ecosystem, root, manifest path)`, strictly.
    pub scopes: Vec<Scope>,
    /// Sorted by `(ecosystem, name, rank, scope)`, strictly. Two entries may
    /// share a name; [`WorkspaceFacts::lookup_name`] reports that.
    pub names: Vec<ScopeName>,
}

/// The resolution scope of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeMatch {
    Unique(ScopeId),
    /// Several scopes at the deepest admitting root govern the file, sorted.
    /// The consumer records [`SCOPE_AMBIGUOUS_BLIND_SPOT`] and applies none.
    Ambiguous(Vec<ScopeId>),
    /// No scope of the ecosystem governs the file.
    Unscoped,
}

/// The answer of a name lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameLookup {
    Unique(ScopeId),
    /// `count` scopes of the best rank carry the name; none is chosen.
    Ambiguous {
        count: usize,
    },
    Missing,
}

/// The scopes one changed path makes stale.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Invalidation {
    /// Sorted, unique. Every file whose scope is listed must resolve again.
    pub scopes: Vec<ScopeId>,
    /// Set when the path can add or remove a scope root: every file of
    /// `ecosystem` under `dir` may change scope, including files that have
    /// no scope today.
    pub rescoped: Option<RescopedSubtree>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescopedSubtree {
    pub ecosystem: Ecosystem,
    pub dir: String,
}

/// A broken invariant of a [`WorkspaceFacts`] table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceFactsError {
    /// `scopes[index]` is not strictly after `scopes[index - 1]` by
    /// `(ecosystem, root, manifest path)`.
    ScopesNotSorted { index: usize },
    /// A root, config path, listed member, entry file or search root is not
    /// a normal repo-relative path.
    BadPath { scope: ScopeId, path: String },
    /// `config_files` is empty, or its first entry is missing, not directly
    /// in `root`, or not a `Root` config name of the scope's ecosystem.
    MissingRootManifest { scope: ScopeId },
    /// A `Listed` membership is not strictly sorted, or lists a file outside
    /// `root`.
    BadMembership { scope: ScopeId },
    /// An alias entry has no replacement.
    EmptyAlias { scope: ScopeId, pattern: String },
    /// The dependency names of `scope` are not strictly sorted.
    DependenciesNotSorted { scope: ScopeId },
    /// A dependency or a name points past the end of `scopes`.
    ScopeOutOfRange { target: ScopeId },
    /// A dependency or a name joins scopes of two ecosystems.
    EcosystemMismatch { scope: ScopeId, target: ScopeId },
    /// `names[index]` is empty or not strictly after `names[index - 1]`.
    NamesNotSorted { index: usize },
}

impl Scope {
    /// The path of the manifest that made this scope; empty only in a row
    /// that fails validation.
    pub fn manifest(&self) -> &str {
        self.config_files
            .first()
            .map_or("", |config| config.path.as_str())
    }

    /// The declared dependency that code in this scope names `name`.
    pub fn dependency(&self, name: &str) -> Option<DependencyTarget> {
        self.dependencies
            .binary_search_by(|dep| dep.name.as_str().cmp(name))
            .ok()
            .map(|i| self.dependencies[i].target)
    }

    /// This scope's aliases as the expander that today's resolver tiers
    /// take, in declaration order.
    pub fn path_aliases(&self) -> PathAliases {
        let mut aliases = PathAliases::new();
        for alias in &self.aliases {
            aliases.add(alias.pattern.as_str(), alias.replacements.clone());
        }
        aliases
    }

    fn reads(&self, path: &str) -> bool {
        self.config_files.iter().any(|config| config.path == path)
    }
}

impl WorkspaceFacts {
    /// The table, when every invariant holds.
    pub fn new(scopes: Vec<Scope>, names: Vec<ScopeName>) -> Result<Self, WorkspaceFactsError> {
        let facts = Self { scopes, names };
        facts.validate()?;
        Ok(facts)
    }

    /// Check every invariant the field docs state. A reader runs it once
    /// on a table it did not build.
    pub fn validate(&self) -> Result<(), WorkspaceFactsError> {
        for (i, pair) in self.scopes.windows(2).enumerate() {
            if scope_key(&pair[0]) >= scope_key(&pair[1]) {
                return Err(WorkspaceFactsError::ScopesNotSorted { index: i + 1 });
            }
        }
        for (id, scope) in self.ids().zip(&self.scopes) {
            self.validate_scope(id, scope)?;
        }
        for (i, entry) in self.names.iter().enumerate() {
            let sorted = i == 0 || name_key(&self.names[i - 1]) < name_key(entry);
            if entry.name.is_empty() || !sorted {
                return Err(WorkspaceFactsError::NamesNotSorted { index: i });
            }
            let target = self
                .scope(entry.scope)
                .ok_or(WorkspaceFactsError::ScopeOutOfRange {
                    target: entry.scope,
                })?;
            if target.ecosystem != entry.ecosystem {
                return Err(WorkspaceFactsError::EcosystemMismatch {
                    scope: entry.scope,
                    target: entry.scope,
                });
            }
        }
        Ok(())
    }

    fn validate_scope(&self, id: ScopeId, scope: &Scope) -> Result<(), WorkspaceFactsError> {
        let bad_path = |path: &str| WorkspaceFactsError::BadPath {
            scope: id,
            path: path.to_string(),
        };
        if !is_normal_dir(&scope.root) {
            return Err(bad_path(&scope.root));
        }
        let listed: &[String] = match &scope.membership {
            Membership::Subtree => &[],
            Membership::Listed(files) => files,
        };
        if let Some(path) = scope
            .config_files
            .iter()
            .map(|config| &config.path)
            .chain(listed)
            .chain(&scope.entry_files)
            .find(|path| !is_normal_path(path))
        {
            return Err(bad_path(path));
        }
        if let Some(dir) = scope.search_roots.iter().find(|dir| !is_normal_dir(dir)) {
            return Err(bad_path(dir));
        }
        match scope.config_files.first() {
            Some(manifest)
                if manifest.presence == ConfigPresence::Read
                    && parent_dir(&manifest.path) == scope.root
                    && Ecosystem::classify_config(file_name(&manifest.path))
                        == Some((scope.ecosystem, ConfigRole::Root)) => {}
            _ => return Err(WorkspaceFactsError::MissingRootManifest { scope: id }),
        }
        if listed.windows(2).any(|pair| pair[0] >= pair[1])
            || listed
                .iter()
                .any(|file| !is_within(parent_dir(file), &scope.root))
        {
            return Err(WorkspaceFactsError::BadMembership { scope: id });
        }
        if let Some(alias) = scope.aliases.iter().find(|a| a.replacements.is_empty()) {
            return Err(WorkspaceFactsError::EmptyAlias {
                scope: id,
                pattern: alias.pattern.clone(),
            });
        }
        if scope
            .dependencies
            .windows(2)
            .any(|pair| pair[0].name >= pair[1].name)
        {
            return Err(WorkspaceFactsError::DependenciesNotSorted { scope: id });
        }
        for dep in &scope.dependencies {
            let DependencyTarget::Workspace(target) = dep.target else {
                continue;
            };
            let target_scope = self
                .scope(target)
                .ok_or(WorkspaceFactsError::ScopeOutOfRange { target })?;
            if target_scope.ecosystem != scope.ecosystem {
                return Err(WorkspaceFactsError::EcosystemMismatch { scope: id, target });
            }
        }
        Ok(())
    }

    pub fn scope(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.get(id.index())
    }

    /// The scopes whose root is exactly `root`, in table order.
    pub fn scopes_at(&self, ecosystem: Ecosystem, root: &str) -> impl Iterator<Item = ScopeId> {
        let start = self
            .scopes
            .partition_point(|scope| (scope.ecosystem, scope.root.as_str()) < (ecosystem, root));
        let len = self.scopes[start..]
            .partition_point(|scope| scope.ecosystem == ecosystem && scope.root == root);
        (start as u32..(start + len) as u32).map(ScopeId)
    }

    /// The resolution scope of the file at `path`. Walk from the file's
    /// directory up to the repo root; at the first directory where some
    /// scope of `ecosystem` admits the file, one admitting scope is
    /// `Unique` and several are `Ambiguous`. `Unscoped` when no directory
    /// has one. The rule reads only the path and the stored rows, so an
    /// indexed file and a new dirty file get the same answer.
    pub fn scope_of(&self, path: &str, ecosystem: Ecosystem) -> ScopeMatch {
        let mut dir = parent_dir(path);
        loop {
            let admitting: Vec<ScopeId> = self
                .scopes_at(ecosystem, dir)
                .filter(|&id| self.scopes[id.index()].membership.admits(path))
                .collect();
            match admitting.len() {
                0 => {}
                1 => return ScopeMatch::Unique(admitting[0]),
                _ => return ScopeMatch::Ambiguous(admitting),
            }
            if dir.is_empty() {
                return ScopeMatch::Unscoped;
            }
            dir = parent_dir(dir);
        }
    }

    /// Every scope at the deepest directory, from `dir` up, that holds a
    /// scope of `ecosystem`, whatever its membership: the conservative set
    /// for a config change in `dir`.
    fn enclosing_scopes(&self, mut dir: &str, ecosystem: Ecosystem) -> Vec<ScopeId> {
        loop {
            let found: Vec<ScopeId> = self.scopes_at(ecosystem, dir).collect();
            if !found.is_empty() || dir.is_empty() {
                return found;
            }
            dir = parent_dir(dir);
        }
    }

    /// The scope that code of `ecosystem` names `name`, across the whole
    /// table. A `Primary` entry beats an `Alternate` one; two entries of the
    /// best rank give `Ambiguous`.
    pub fn lookup_name(&self, ecosystem: Ecosystem, name: &str) -> NameLookup {
        let start = self
            .names
            .partition_point(|entry| (entry.ecosystem, entry.name.as_str()) < (ecosystem, name));
        let rest = &self.names[start..];
        let matching = &rest
            [..rest.partition_point(|entry| entry.ecosystem == ecosystem && entry.name == name)];
        let primaries = matching.partition_point(|entry| entry.rank == NameRank::Primary);
        let best = if primaries > 0 {
            &matching[..primaries]
        } else {
            matching
        };
        match best {
            [] => NameLookup::Missing,
            [only] => NameLookup::Unique(only.scope),
            _ => NameLookup::Ambiguous { count: best.len() },
        }
    }

    /// The ecosystem of `path` when it is a resolution config: its file
    /// name is a config name, or some scope lists it in `config_files`,
    /// present or missing. This is one member of C3's class set; the same
    /// path can also be source by C5's routing. Cost: one match, then one
    /// pass over every scope's config paths.
    pub fn config_ecosystem(&self, path: &str) -> Option<Ecosystem> {
        Ecosystem::classify_config(file_name(path))
            .map(|(ecosystem, _)| ecosystem)
            .or_else(|| {
                self.scopes
                    .iter()
                    .find(|scope| scope.reads(path))
                    .map(|scope| scope.ecosystem)
            })
    }

    /// The scopes that a change to `path` (added, modified or deleted) makes
    /// stale: the scopes that tried to read it, present or missing; when its
    /// name is a config name, the scopes at the nearest enclosing root and
    /// every scope at or below its directory; then one hop of scopes that
    /// depend on any of those. Empty for a path that is no resolution
    /// config. A changed scope can also gain a name that another scope
    /// declares as `External`; [`WorkspaceFacts::dependents_of_name`]
    /// answers that once the new config is read.
    pub fn invalidated_by(&self, path: &str) -> Invalidation {
        let mut scopes: Vec<ScopeId> = self
            .ids()
            .zip(&self.scopes)
            .filter(|(_, scope)| scope.reads(path))
            .map(|(id, _)| id)
            .collect();
        let mut rescoped = None;
        if let Some((ecosystem, role)) = Ecosystem::classify_config(file_name(path)) {
            let dir = parent_dir(path);
            scopes.extend(self.enclosing_scopes(dir, ecosystem));
            scopes.extend(
                self.ids()
                    .zip(&self.scopes)
                    .filter(|(_, scope)| {
                        scope.ecosystem == ecosystem && is_within(&scope.root, dir)
                    })
                    .map(|(id, _)| id),
            );
            if role == ConfigRole::Root {
                rescoped = Some(RescopedSubtree {
                    ecosystem,
                    dir: dir.to_string(),
                });
            }
        }
        scopes.sort_unstable();
        scopes.dedup();
        let dependents: Vec<ScopeId> = self
            .ids()
            .zip(&self.scopes)
            .filter(|(_, scope)| {
                scope.dependencies.iter().any(|dep| match dep.target {
                    DependencyTarget::Workspace(target) => scopes.binary_search(&target).is_ok(),
                    DependencyTarget::External => false,
                })
            })
            .map(|(id, _)| id)
            .collect();
        scopes.extend(dependents);
        scopes.sort_unstable();
        scopes.dedup();
        Invalidation { scopes, rescoped }
    }

    /// The scopes of `ecosystem` that declare a dependency named `name`,
    /// whatever it resolves to today. Sorted.
    pub fn dependents_of_name(&self, ecosystem: Ecosystem, name: &str) -> Vec<ScopeId> {
        self.ids()
            .zip(&self.scopes)
            .filter(|(_, scope)| scope.ecosystem == ecosystem && scope.dependency(name).is_some())
            .map(|(id, _)| id)
            .collect()
    }

    fn ids(&self) -> impl Iterator<Item = ScopeId> {
        (0..self.scopes.len() as u32).map(ScopeId)
    }
}

fn scope_key(scope: &Scope) -> (Ecosystem, &str, &str) {
    (scope.ecosystem, scope.root.as_str(), scope.manifest())
}

fn name_key(entry: &ScopeName) -> (Ecosystem, &str, NameRank, ScopeId) {
    (
        entry.ecosystem,
        entry.name.as_str(),
        entry.rank,
        entry.scope,
    )
}

pub(crate) fn is_normal_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn is_normal_dir(dir: &str) -> bool {
    dir.is_empty() || is_normal_path(dir)
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// `root` is `dir` or below it. Compares whole segments, so `packages/ab`
/// is not within `packages/a`.
fn is_within(root: &str, dir: &str) -> bool {
    dir.is_empty()
        || root
            .strip_prefix(dir)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn read(path: &str) -> ConfigRead {
        ConfigRead {
            path: path.to_string(),
            presence: ConfigPresence::Read,
        }
    }

    fn missing(path: &str) -> ConfigRead {
        ConfigRead {
            path: path.to_string(),
            presence: ConfigPresence::Missing,
        }
    }

    fn scope(ecosystem: Ecosystem, root: &str, config_files: &[&str]) -> Scope {
        Scope {
            ecosystem,
            root: root.to_string(),
            config_files: config_files.iter().map(|path| read(path)).collect(),
            membership: Membership::Subtree,
            entry_files: Vec::new(),
            aliases: Vec::new(),
            search_roots: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    fn dep(name: &str, target: DependencyTarget) -> Dependency {
        Dependency {
            name: name.to_string(),
            target,
        }
    }

    fn name(ecosystem: Ecosystem, name: &str, rank: NameRank, scope: u32) -> ScopeName {
        ScopeName {
            ecosystem,
            name: name.to_string(),
            rank,
            scope: ScopeId(scope),
        }
    }

    /// 0: Cargo `crates/a` (depends on `b` and on external `serde`),
    /// 1: Cargo `crates/b`, 2: Node root, 3: Node `packages/a` (extends
    /// `tsconfig.base.json` and the missing `config/base.json`).
    fn fixture_scopes() -> Vec<Scope> {
        let mut crate_a = scope(
            Ecosystem::Cargo,
            "crates/a",
            &["crates/a/Cargo.toml", "Cargo.toml"],
        );
        crate_a.entry_files = strings(&["crates/a/src/lib.rs", "crates/a/src/bin/tool.rs"]);
        crate_a.dependencies = vec![
            dep("b", DependencyTarget::Workspace(ScopeId(1))),
            dep("serde", DependencyTarget::External),
        ];
        let mut crate_b = scope(
            Ecosystem::Cargo,
            "crates/b",
            &["crates/b/Cargo.toml", "Cargo.toml"],
        );
        crate_b.entry_files = strings(&["crates/b/src/lib.rs"]);
        let node_root = scope(Ecosystem::Node, "", &["package.json"]);
        let mut package_a = scope(
            Ecosystem::Node,
            "packages/a",
            &["packages/a/tsconfig.json", "tsconfig.base.json"],
        );
        package_a.config_files.push(missing("config/base.json"));
        package_a.aliases = vec![
            ScopeAlias {
                pattern: "@/*".to_string(),
                replacements: strings(&["packages/a/src/*"]),
            },
            ScopeAlias {
                pattern: "@/*".to_string(),
                replacements: strings(&["packages/a/lib/*"]),
            },
        ];
        vec![crate_a, crate_b, node_root, package_a]
    }

    fn fixture_names() -> Vec<ScopeName> {
        vec![
            name(Ecosystem::Cargo, "a", NameRank::Primary, 0),
            name(Ecosystem::Cargo, "b", NameRank::Primary, 1),
            name(Ecosystem::Cargo, "b-pkg", NameRank::Alternate, 1),
            name(Ecosystem::Node, "@repo/a", NameRank::Primary, 3),
        ]
    }

    fn fixture() -> WorkspaceFacts {
        WorkspaceFacts::new(fixture_scopes(), fixture_names()).unwrap()
    }

    /// 0: `svc/App.csproj`, 1: `svc/Tests.csproj`, both in `svc`, with the
    /// given memberships.
    fn two_projects(app: Membership, tests: Membership) -> WorkspaceFacts {
        let mut app_scope = scope(Ecosystem::DotNet, "svc", &["svc/App.csproj"]);
        app_scope.membership = app;
        let mut tests_scope = scope(Ecosystem::DotNet, "svc", &["svc/Tests.csproj"]);
        tests_scope.membership = tests;
        WorkspaceFacts::new(vec![app_scope, tests_scope], Vec::new()).unwrap()
    }

    #[test]
    fn test_for_language_rust_returns_cargo() {
        assert_eq!(
            Ecosystem::for_language(Language::Rust),
            Some(Ecosystem::Cargo)
        );
        assert_eq!(
            Ecosystem::for_language(Language::JavaScript),
            Some(Ecosystem::Node)
        );
    }

    #[test]
    fn test_for_language_components_return_node() {
        for language in [Language::Vue, Language::Astro, Language::Svelte] {
            assert_eq!(
                Ecosystem::for_language(language),
                Some(Ecosystem::Node),
                "{language:?}"
            );
        }
    }

    #[test]
    fn test_for_language_without_facts_returns_none() {
        assert_eq!(Ecosystem::for_language(Language::Yaml), None);
        assert_eq!(Ecosystem::for_language(Language::Java), None);
    }

    #[test]
    fn test_for_language_every_language_maps_nine_to_an_ecosystem() {
        let mapped = Language::ALL
            .iter()
            .filter(|&&language| Ecosystem::for_language(language).is_some())
            .count();
        assert_eq!(mapped, 9);
    }

    #[test]
    fn test_classify_config_manifest_names_return_root() {
        for (file, ecosystem) in [
            ("Cargo.toml", Ecosystem::Cargo),
            ("tsconfig.json", Ecosystem::Node),
            ("package.json", Ecosystem::Node),
            ("go.mod", Ecosystem::Go),
            ("pyproject.toml", Ecosystem::Python),
            ("App.csproj", Ecosystem::DotNet),
        ] {
            assert_eq!(
                Ecosystem::classify_config(file),
                Some((ecosystem, ConfigRole::Root)),
                "{file}"
            );
        }
    }

    #[test]
    fn test_classify_config_contributing_names_return_contributing() {
        assert_eq!(
            Ecosystem::classify_config("go.work"),
            Some((Ecosystem::Go, ConfigRole::Contributing))
        );
        assert_eq!(
            Ecosystem::classify_config("webpack.config.ts"),
            Some((Ecosystem::Node, ConfigRole::Contributing))
        );
    }

    #[test]
    fn test_classify_config_other_names_return_none() {
        for file in [".csproj", "tsconfig.base.json", "main.rs", "Cargo.lock", ""] {
            assert_eq!(Ecosystem::classify_config(file), None, "{file}");
        }
    }

    #[test]
    fn test_new_valid_table_returns_ok() {
        let facts = fixture();
        assert_eq!(facts.scopes.len(), 4);
        assert_eq!(
            WorkspaceFacts::new(Vec::new(), Vec::new()),
            Ok(WorkspaceFacts::default())
        );
    }

    #[test]
    fn test_new_unsorted_scopes_returns_scopes_not_sorted() {
        let mut scopes = fixture_scopes();
        scopes.swap(0, 1);
        assert_eq!(
            WorkspaceFacts::new(scopes, Vec::new()),
            Err(WorkspaceFactsError::ScopesNotSorted { index: 1 })
        );
    }

    #[test]
    fn test_new_duplicate_manifest_returns_scopes_not_sorted() {
        let scopes = vec![
            scope(Ecosystem::Go, "svc", &["svc/go.mod"]),
            scope(Ecosystem::Go, "svc", &["svc/go.mod"]),
        ];
        assert_eq!(
            WorkspaceFacts::new(scopes, Vec::new()),
            Err(WorkspaceFactsError::ScopesNotSorted { index: 1 })
        );
    }

    #[test]
    fn test_new_two_manifests_in_one_root_returns_ok() {
        let facts = two_projects(Membership::Subtree, Membership::Subtree);
        assert_eq!(facts.scopes[0].manifest(), "svc/App.csproj");
        assert_eq!(facts.scopes[1].manifest(), "svc/Tests.csproj");
    }

    #[test]
    fn test_new_abnormal_paths_return_bad_path() {
        for (root, manifest, entry) in [
            ("svc/", "svc/go.mod", "svc/main.go"),
            ("./svc", "./svc/go.mod", "svc/main.go"),
            ("svc", "svc/go.mod", "svc/../main.go"),
            ("svc", "svc\\go.mod", "svc/main.go"),
            ("svc", "svc/go.mod", "/svc/main.go"),
        ] {
            let mut row = scope(Ecosystem::Go, root, &[manifest]);
            row.entry_files = strings(&[entry]);
            assert!(
                matches!(
                    WorkspaceFacts::new(vec![row], Vec::new()),
                    Err(WorkspaceFactsError::BadPath { .. })
                ),
                "{root} {manifest} {entry}"
            );
        }
    }

    #[test]
    fn test_new_search_root_with_dot_returns_bad_path() {
        let mut row = scope(Ecosystem::Python, "", &["pyproject.toml"]);
        row.search_roots = strings(&["", "src", "./lib"]);
        assert_eq!(
            WorkspaceFacts::new(vec![row], Vec::new()),
            Err(WorkspaceFactsError::BadPath {
                scope: ScopeId(0),
                path: "./lib".to_string()
            })
        );
    }

    #[test]
    fn test_new_manifest_outside_root_returns_missing_root_manifest() {
        for config in [
            &["Cargo.toml"][..],
            &[][..],
            &["crates/a/sub/Cargo.toml"][..],
        ] {
            let row = scope(Ecosystem::Cargo, "crates/a", config);
            assert_eq!(
                WorkspaceFacts::new(vec![row], Vec::new()),
                Err(WorkspaceFactsError::MissingRootManifest { scope: ScopeId(0) }),
                "{config:?}"
            );
        }
    }

    #[test]
    fn test_new_first_config_not_root_of_ecosystem_returns_missing_root_manifest() {
        for first in [
            "crates/a/package.json",
            "crates/a/README.md",
            "crates/a/go.work",
        ] {
            let row = scope(Ecosystem::Cargo, "crates/a", &[first]);
            assert_eq!(
                WorkspaceFacts::new(vec![row], Vec::new()),
                Err(WorkspaceFactsError::MissingRootManifest { scope: ScopeId(0) }),
                "{first}"
            );
        }
    }

    #[test]
    fn test_new_missing_first_config_returns_missing_root_manifest() {
        let mut row = scope(Ecosystem::Cargo, "crates/a", &[]);
        row.config_files = vec![missing("crates/a/Cargo.toml")];
        assert_eq!(
            WorkspaceFacts::new(vec![row], Vec::new()),
            Err(WorkspaceFactsError::MissingRootManifest { scope: ScopeId(0) })
        );
    }

    #[test]
    fn test_new_bad_listed_membership_returns_bad_membership() {
        for files in [
            &["svc/b.cs", "svc/a.cs"][..],
            &["svc/a.cs", "svc/a.cs"][..],
            &["other/a.cs"][..],
            &["svcx/a.cs"][..],
        ] {
            let mut row = scope(Ecosystem::DotNet, "svc", &["svc/App.csproj"]);
            row.membership = Membership::Listed(strings(files));
            assert_eq!(
                WorkspaceFacts::new(vec![row], Vec::new()),
                Err(WorkspaceFactsError::BadMembership { scope: ScopeId(0) }),
                "{files:?}"
            );
        }
    }

    #[test]
    fn test_new_listed_member_with_dotdot_returns_bad_path() {
        let mut row = scope(Ecosystem::DotNet, "svc", &["svc/App.csproj"]);
        row.membership = Membership::Listed(strings(&["svc/../a.cs"]));
        assert_eq!(
            WorkspaceFacts::new(vec![row], Vec::new()),
            Err(WorkspaceFactsError::BadPath {
                scope: ScopeId(0),
                path: "svc/../a.cs".to_string()
            })
        );
    }

    #[test]
    fn test_new_alias_without_replacement_returns_empty_alias() {
        let mut row = scope(Ecosystem::Node, "", &["tsconfig.json"]);
        row.aliases = vec![ScopeAlias {
            pattern: "@/*".to_string(),
            replacements: Vec::new(),
        }];
        assert_eq!(
            WorkspaceFacts::new(vec![row], Vec::new()),
            Err(WorkspaceFactsError::EmptyAlias {
                scope: ScopeId(0),
                pattern: "@/*".to_string()
            })
        );
    }

    #[test]
    fn test_new_unsorted_dependencies_returns_dependencies_not_sorted() {
        let mut scopes = fixture_scopes();
        scopes[0].dependencies.reverse();
        assert_eq!(
            WorkspaceFacts::new(scopes, fixture_names()),
            Err(WorkspaceFactsError::DependenciesNotSorted { scope: ScopeId(0) })
        );
        let mut scopes = fixture_scopes();
        scopes[0]
            .dependencies
            .push(dep("serde", DependencyTarget::External));
        assert_eq!(
            WorkspaceFacts::new(scopes, fixture_names()),
            Err(WorkspaceFactsError::DependenciesNotSorted { scope: ScopeId(0) })
        );
    }

    #[test]
    fn test_new_dependency_past_end_returns_scope_out_of_range() {
        let mut scopes = fixture_scopes();
        scopes[0].dependencies[0].target = DependencyTarget::Workspace(ScopeId(9));
        assert_eq!(
            WorkspaceFacts::new(scopes, fixture_names()),
            Err(WorkspaceFactsError::ScopeOutOfRange { target: ScopeId(9) })
        );
    }

    #[test]
    fn test_new_dependency_across_ecosystems_returns_ecosystem_mismatch() {
        let mut scopes = fixture_scopes();
        scopes[0].dependencies[0].target = DependencyTarget::Workspace(ScopeId(3));
        assert_eq!(
            WorkspaceFacts::new(scopes, fixture_names()),
            Err(WorkspaceFactsError::EcosystemMismatch {
                scope: ScopeId(0),
                target: ScopeId(3)
            })
        );
    }

    #[test]
    fn test_new_unsorted_names_returns_names_not_sorted() {
        let mut names = fixture_names();
        names.swap(0, 1);
        assert_eq!(
            WorkspaceFacts::new(fixture_scopes(), names),
            Err(WorkspaceFactsError::NamesNotSorted { index: 1 })
        );
        let mut names = fixture_names();
        names[0].name.clear();
        assert_eq!(
            WorkspaceFacts::new(fixture_scopes(), names),
            Err(WorkspaceFactsError::NamesNotSorted { index: 0 })
        );
    }

    #[test]
    fn test_new_name_of_other_ecosystem_returns_ecosystem_mismatch() {
        let mut names = fixture_names();
        names[3].scope = ScopeId(1);
        assert_eq!(
            WorkspaceFacts::new(fixture_scopes(), names),
            Err(WorkspaceFactsError::EcosystemMismatch {
                scope: ScopeId(1),
                target: ScopeId(1)
            })
        );
    }

    #[test]
    fn test_new_name_past_end_returns_scope_out_of_range() {
        let mut names = fixture_names();
        names[3].scope = ScopeId(4);
        assert_eq!(
            WorkspaceFacts::new(fixture_scopes(), names),
            Err(WorkspaceFactsError::ScopeOutOfRange { target: ScopeId(4) })
        );
    }

    #[test]
    fn test_scopes_at_exact_root_returns_every_manifest() {
        let facts = fixture();
        assert_eq!(
            facts
                .scopes_at(Ecosystem::Cargo, "crates/b")
                .collect::<Vec<_>>(),
            vec![ScopeId(1)]
        );
        assert_eq!(facts.scopes_at(Ecosystem::Node, "crates/b").count(), 0);
        let projects = two_projects(Membership::Subtree, Membership::Subtree);
        assert_eq!(
            projects
                .scopes_at(Ecosystem::DotNet, "svc")
                .collect::<Vec<_>>(),
            vec![ScopeId(0), ScopeId(1)]
        );
    }

    #[test]
    fn test_scope_of_nested_file_returns_deepest_root() {
        let facts = fixture();
        assert_eq!(
            facts.scope_of("crates/a/src/deep/mod.rs", Ecosystem::Cargo),
            ScopeMatch::Unique(ScopeId(0))
        );
        assert_eq!(
            facts.scope_of("packages/a/src/util.ts", Ecosystem::Node),
            ScopeMatch::Unique(ScopeId(3))
        );
    }

    #[test]
    fn test_scope_of_sibling_prefix_dir_returns_root_scope() {
        let facts = fixture();
        assert_eq!(
            facts.scope_of("packages/ab/main.ts", Ecosystem::Node),
            ScopeMatch::Unique(ScopeId(2))
        );
        assert_eq!(
            facts.scope_of("main.ts", Ecosystem::Node),
            ScopeMatch::Unique(ScopeId(2))
        );
    }

    #[test]
    fn test_scope_of_file_outside_every_root_returns_unscoped() {
        let facts = fixture();
        assert_eq!(
            facts.scope_of("tools/x.rs", Ecosystem::Cargo),
            ScopeMatch::Unscoped
        );
        assert_eq!(
            facts.scope_of("crates/a/main.go", Ecosystem::Go),
            ScopeMatch::Unscoped
        );
    }

    #[test]
    fn test_scope_of_two_subtree_projects_in_one_root_returns_ambiguous() {
        let facts = two_projects(Membership::Subtree, Membership::Subtree);
        assert_eq!(
            facts.scope_of("svc/Program.cs", Ecosystem::DotNet),
            ScopeMatch::Ambiguous(vec![ScopeId(0), ScopeId(1)])
        );
    }

    #[test]
    fn test_scope_of_listed_projects_in_one_root_returns_member_scope() {
        let facts = two_projects(
            Membership::Listed(strings(&["svc/Program.cs"])),
            Membership::Listed(strings(&["svc/ProgramTests.cs"])),
        );
        assert_eq!(
            facts.scope_of("svc/Program.cs", Ecosystem::DotNet),
            ScopeMatch::Unique(ScopeId(0))
        );
        assert_eq!(
            facts.scope_of("svc/ProgramTests.cs", Ecosystem::DotNet),
            ScopeMatch::Unique(ScopeId(1))
        );
        assert_eq!(
            facts.scope_of("svc/Orphan.cs", Ecosystem::DotNet),
            ScopeMatch::Unscoped
        );
    }

    #[test]
    fn test_scope_of_file_not_listed_deeper_returns_ancestor_scope() {
        let root = scope(Ecosystem::Node, "", &["tsconfig.json"]);
        let mut package = scope(Ecosystem::Node, "pkg", &["pkg/tsconfig.json"]);
        package.membership = Membership::Listed(strings(&["pkg/src/a.ts"]));
        let facts = WorkspaceFacts::new(vec![root, package], Vec::new()).unwrap();
        assert_eq!(
            facts.scope_of("pkg/src/a.ts", Ecosystem::Node),
            ScopeMatch::Unique(ScopeId(1))
        );
        assert_eq!(
            facts.scope_of("pkg/scripts/b.ts", Ecosystem::Node),
            ScopeMatch::Unique(ScopeId(0))
        );
    }

    #[test]
    fn test_admits_listed_membership_returns_listed_only() {
        let listed = Membership::Listed(strings(&["a/x.ts", "a/y.ts"]));
        assert!(listed.admits("a/y.ts"));
        assert!(!listed.admits("a/z.ts"));
        assert!(Membership::Subtree.admits("anything.ts"));
    }

    #[test]
    fn test_lookup_name_primary_returns_unique() {
        let facts = fixture();
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "b"),
            NameLookup::Unique(ScopeId(1))
        );
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "b-pkg"),
            NameLookup::Unique(ScopeId(1))
        );
    }

    #[test]
    fn test_lookup_name_primary_and_alternate_returns_primary() {
        let names = vec![
            name(Ecosystem::Cargo, "a", NameRank::Primary, 0),
            name(Ecosystem::Cargo, "b", NameRank::Primary, 1),
            name(Ecosystem::Cargo, "b", NameRank::Alternate, 0),
        ];
        let facts = WorkspaceFacts::new(fixture_scopes(), names).unwrap();
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "b"),
            NameLookup::Unique(ScopeId(1))
        );
    }

    #[test]
    fn test_lookup_name_two_primaries_returns_ambiguous() {
        let names = vec![
            name(Ecosystem::Cargo, "dup", NameRank::Primary, 0),
            name(Ecosystem::Cargo, "dup", NameRank::Primary, 1),
        ];
        let facts = WorkspaceFacts::new(fixture_scopes(), names).unwrap();
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "dup"),
            NameLookup::Ambiguous { count: 2 }
        );
    }

    #[test]
    fn test_lookup_name_unknown_or_other_ecosystem_returns_missing() {
        let facts = fixture();
        assert_eq!(
            facts.lookup_name(Ecosystem::Cargo, "serde"),
            NameLookup::Missing
        );
        assert_eq!(facts.lookup_name(Ecosystem::Node, "a"), NameLookup::Missing);
        assert_eq!(
            WorkspaceFacts::default().lookup_name(Ecosystem::Go, "x"),
            NameLookup::Missing
        );
    }

    #[test]
    fn test_dependency_declared_names_return_target() {
        let facts = fixture();
        let crate_a = facts.scope(ScopeId(0)).unwrap();
        assert_eq!(
            crate_a.dependency("b"),
            Some(DependencyTarget::Workspace(ScopeId(1)))
        );
        assert_eq!(
            crate_a.dependency("serde"),
            Some(DependencyTarget::External)
        );
        assert_eq!(crate_a.dependency("tokio"), None);
    }

    #[test]
    fn test_path_aliases_scope_rows_expand_in_declaration_order() {
        let facts = fixture();
        let aliases = facts.scope(ScopeId(3)).unwrap().path_aliases();
        let mut got = Vec::new();
        aliases.expand("@/util", |candidate| {
            got.push(candidate.to_string());
            true
        });
        assert_eq!(got, ["packages/a/src/util", "packages/a/lib/util"]);
        assert!(facts.scope(ScopeId(0)).unwrap().path_aliases().is_empty());
    }

    #[test]
    fn test_config_ecosystem_config_paths_return_ecosystem() {
        let facts = fixture();
        assert_eq!(
            facts.config_ecosystem("crates/a/Cargo.toml"),
            Some(Ecosystem::Cargo)
        );
        assert_eq!(
            facts.config_ecosystem("new/pkg/package.json"),
            Some(Ecosystem::Node)
        );
        assert_eq!(
            facts.config_ecosystem("tsconfig.base.json"),
            Some(Ecosystem::Node)
        );
        assert_eq!(
            facts.config_ecosystem("web/webpack.config.ts"),
            Some(Ecosystem::Node)
        );
    }

    #[test]
    fn test_config_ecosystem_missing_extends_target_returns_node() {
        let facts = fixture();
        assert_eq!(
            facts.config_ecosystem("config/base.json"),
            Some(Ecosystem::Node)
        );
    }

    #[test]
    fn test_config_ecosystem_source_path_returns_none() {
        let facts = fixture();
        assert_eq!(facts.config_ecosystem("crates/a/src/lib.rs"), None);
        assert_eq!(facts.config_ecosystem("data/rows.csv"), None);
    }

    #[test]
    fn test_invalidated_by_workspace_manifest_returns_members() {
        let facts = fixture();
        assert_eq!(
            facts.invalidated_by("Cargo.toml"),
            Invalidation {
                scopes: vec![ScopeId(0), ScopeId(1)],
                rescoped: Some(RescopedSubtree {
                    ecosystem: Ecosystem::Cargo,
                    dir: String::new()
                }),
            }
        );
    }

    #[test]
    fn test_invalidated_by_member_manifest_returns_owner_and_dependents() {
        let facts = fixture();
        assert_eq!(
            facts.invalidated_by("crates/b/Cargo.toml").scopes,
            vec![ScopeId(0), ScopeId(1)]
        );
        assert_eq!(
            facts.invalidated_by("crates/a/Cargo.toml").scopes,
            vec![ScopeId(0)]
        );
    }

    #[test]
    fn test_invalidated_by_extended_file_returns_reader_without_rescope() {
        let facts = fixture();
        assert_eq!(
            facts.invalidated_by("tsconfig.base.json"),
            Invalidation {
                scopes: vec![ScopeId(3)],
                rescoped: None,
            }
        );
    }

    #[test]
    fn test_invalidated_by_missing_extends_target_appears_returns_reader() {
        let facts = fixture();
        assert_eq!(
            facts.invalidated_by("config/base.json"),
            Invalidation {
                scopes: vec![ScopeId(3)],
                rescoped: None,
            }
        );
    }

    #[test]
    fn test_invalidated_by_added_root_manifest_returns_enclosing_and_subtree() {
        let facts = fixture();
        assert_eq!(
            facts.invalidated_by("packages/b/package.json"),
            Invalidation {
                scopes: vec![ScopeId(2)],
                rescoped: Some(RescopedSubtree {
                    ecosystem: Ecosystem::Node,
                    dir: "packages/b".to_string()
                }),
            }
        );
    }

    #[test]
    fn test_invalidated_by_project_beside_projects_returns_every_project_at_root() {
        let facts = two_projects(Membership::Subtree, Membership::Subtree);
        assert_eq!(
            facts.invalidated_by("svc/Bench.csproj"),
            Invalidation {
                scopes: vec![ScopeId(0), ScopeId(1)],
                rescoped: Some(RescopedSubtree {
                    ecosystem: Ecosystem::DotNet,
                    dir: "svc".to_string()
                }),
            }
        );
    }

    #[test]
    fn test_invalidated_by_contributing_config_returns_scopes_without_rescope() {
        let facts = fixture();
        assert_eq!(
            facts.invalidated_by("packages/jest.config.js"),
            Invalidation {
                scopes: vec![ScopeId(2), ScopeId(3)],
                rescoped: None,
            }
        );
    }

    #[test]
    fn test_invalidated_by_source_file_returns_empty() {
        let facts = fixture();
        assert_eq!(
            facts.invalidated_by("crates/a/src/lib.rs"),
            Invalidation::default()
        );
    }

    #[test]
    fn test_dependents_of_name_declared_names_return_declarers() {
        let facts = fixture();
        assert_eq!(
            facts.dependents_of_name(Ecosystem::Cargo, "serde"),
            vec![ScopeId(0)]
        );
        assert_eq!(
            facts.dependents_of_name(Ecosystem::Node, "serde"),
            Vec::<ScopeId>::new()
        );
    }

    #[test]
    fn test_is_within_segment_boundary_returns_false_for_prefix_sibling() {
        assert!(is_within("packages/a", "packages/a"));
        assert!(is_within("packages/a/x", "packages/a"));
        assert!(is_within("packages/a", ""));
        assert!(!is_within("packages/ab", "packages/a"));
    }

    #[test]
    fn test_rkyv_roundtrip_fixture_returns_equal_table() {
        let mut scopes = fixture_scopes();
        scopes[3].membership = Membership::Listed(strings(&["packages/a/src/util.ts"]));
        let facts = WorkspaceFacts::new(scopes, fixture_names()).unwrap();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&facts).unwrap();
        let archived = rkyv::access::<ArchivedWorkspaceFacts, rkyv::rancor::Error>(&bytes).unwrap();
        let back = rkyv::deserialize::<WorkspaceFacts, rkyv::rancor::Error>(archived).unwrap();
        assert_eq!(back, facts);
        assert_eq!(back.validate(), Ok(()));
    }
}
