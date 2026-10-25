//! C1 identity: declaration identity, lexical parent, the target request
//! syntax, the typed target result, and the route and handler keys.
//!
//! Design note: `docs/specs/contracts/C1-identity.md`. `assign_file` numbers
//! every file-level uid site (slice R2, through `analyzer::lexical`); the
//! target and route types wait for their consuming slices.

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::fmt;

use rustc_hash::FxHashMap;
use xxhash_rust::xxh3::Xxh3;

use crate::analyzer::types::{innermost_enclosing, Span};
use crate::error::EcpError;
use crate::graph::NodeKind;
use crate::uid;

// ── Declaration identity ────────────────────────────────────────────────

/// First byte of a scoped uid stream. A `uid::compute` stream starts with an
/// ASCII letter (`NodeKind::as_str`), and a `uid::overload` stream is 12
/// bytes long while a scoped stream is at least 14, so no two streams are
/// equal.
const SCOPED_TAG: u8 = 0x01;
/// First byte of an anchor stream with a discriminator (at least 13 bytes).
const ANCHOR_TAG: u8 = 0x02;

/// A semantic fact that tells apart two declarations whose kind, path, owner
/// and name are equal. It enters the anchor, never the declaration's own
/// uid. Every scoped descendant hashes the anchor, so adding a discriminator
/// to a declaration changes the stored uid of each scoped node below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Discriminator<'a> {
    /// The full trait text of the Rust `impl Trait for Type` block that
    /// declares the node, generic arguments included, as `impl_trait_name`
    /// produces it: `Display`, `Conv<u8>`.
    ImplTrait(&'a str),
    /// The parameter type list of an overload, as the parser normalises it.
    Signature(&'a str),
    /// A producer-specific semantic tag, such as the `kind` of a Kubernetes
    /// resource that shares its `metadata.name` with another.
    Tag(&'a str),
}

impl<'a> Discriminator<'a> {
    /// The stored variant byte: the anchor stream hashes it, and the L2
    /// discriminator table stores it beside the text.
    pub fn stored_tag(self) -> u8 {
        match self {
            Self::ImplTrait(_) => 1,
            Self::Signature(_) => 2,
            Self::Tag(_) => 3,
        }
    }

    /// The inverse of [`Discriminator::stored_tag`] plus [`Discriminator::text`].
    /// `None` for an unknown byte.
    pub fn from_stored(tag: u8, text: &'a str) -> Option<Self> {
        match tag {
            1 => Some(Self::ImplTrait(text)),
            2 => Some(Self::Signature(text)),
            3 => Some(Self::Tag(text)),
            _ => None,
        }
    }

    pub fn text(self) -> &'a str {
        match self {
            Self::ImplTrait(t) | Self::Signature(t) | Self::Tag(t) => t,
        }
    }
}

/// Where a declaration lives for its uid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The uid hashes `(kind, path, owner, name)`, exactly as
    /// `uid::compute` does today.
    Unscoped,
    /// A callable body declares the node. The uid hashes the parent's
    /// anchor, so it does not move when an earlier sibling of the parent
    /// moves.
    Lexical { parent_anchor: u64 },
}

/// Why a declaration key or a file's identities cannot be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// The path holds a `\`; see [`canonical_path`].
    NonCanonicalPath(String),
    /// A discriminator with empty text separates nothing.
    EmptyDiscriminator,
    /// The declaration's lexical parent is out of range or forms a cycle.
    BadParent { decl: u32 },
    /// A lexically scoped declaration carries an owner (invariant I1).
    ScopedWithOwner { decl: u32 },
    /// A kind that keeps overloads is never scoped (invariant I3). `decl` is
    /// the row index when `assign_file` finds it, `None` from
    /// [`DeclKey::scoped`].
    ScopedOverloadKind { kind: NodeKind, decl: Option<u32> },
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonCanonicalPath(p) => write!(f, "path is not canonical: {p:?}"),
            Self::EmptyDiscriminator => f.write_str("discriminator text is empty"),
            Self::BadParent { decl } => write!(f, "declaration {decl} has a bad lexical parent"),
            Self::ScopedWithOwner { decl } => {
                write!(f, "scoped declaration {decl} carries an owner")
            }
            Self::ScopedOverloadKind { kind, decl } => {
                write!(
                    f,
                    "a {} keeps overloads and cannot be scoped (declaration {decl:?})",
                    kind.as_str()
                )
            }
        }
    }
}

impl std::error::Error for IdentityError {}

/// The repo-relative, `/`-separated spelling of a path, as the builder
/// stores it: `builder.rs` replaces `\` with `/` and changes nothing else.
/// A leading `./` is part of the name: a Unix file named `.\a.rs` is stored
/// `./a.rs`. Borrowed when unchanged.
pub fn canonical_path(path: &str) -> Cow<'_, str> {
    match path.contains('\\') {
        true => Cow::Owned(path.replace('\\', "/")),
        false => Cow::Borrowed(path),
    }
}

/// The matching form of an owner or qualifier segment: every generic
/// argument list removed and outer whitespace trimmed, so a request `Walk`
/// matches the owner `Walk<'s, 't>`. Matching only: identity keeps the owner
/// text as the producer wrote it, so `Conv<u8>` and `Conv<u16>` stay two
/// owners. A text that is all generic list (`<anonymous:3:4>`) keeps its
/// text. Borrowed when unchanged.
pub fn owner_match_form(owner: &str) -> Cow<'_, str> {
    if !owner.contains('<') && owner.trim().len() == owner.len() {
        return Cow::Borrowed(owner);
    }
    let mut out = String::with_capacity(owner.len());
    let mut depth = 0u32;
    for c in owner.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    match out.trim() {
        "" => Cow::Borrowed(owner),
        trimmed => Cow::Owned(trimmed.to_string()),
    }
}

fn check_path(path: &str) -> Result<(), IdentityError> {
    match path.contains('\\') {
        true => Err(IdentityError::NonCanonicalPath(path.to_string())),
        false => Ok(()),
    }
}

/// The identity of one declaration. Construct it with [`DeclKey::unscoped`]
/// or [`DeclKey::scoped`]; both validate the path spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclKey<'a> {
    kind: NodeKind,
    path: &'a str,
    owner: Option<&'a str>,
    name: &'a str,
    scope: Scope,
    discriminator: Option<Discriminator<'a>>,
}

impl<'a> DeclKey<'a> {
    /// A declaration that no callable body declares. The owner text stays as
    /// the producer wrote it, generic arguments included (`Conv<u8>`), so
    /// the uid is today's. An empty owner is no owner, as `uid::compute`
    /// already treats it.
    pub fn unscoped(
        kind: NodeKind,
        path: &'a str,
        owner: Option<&'a str>,
        name: &'a str,
    ) -> Result<Self, IdentityError> {
        check_path(path)?;
        Ok(Self::unscoped_unchecked(kind, path, owner, name))
    }

    /// [`DeclKey::unscoped`] for a path the caller has already checked.
    fn unscoped_unchecked(
        kind: NodeKind,
        path: &'a str,
        owner: Option<&'a str>,
        name: &'a str,
    ) -> Self {
        Self {
            kind,
            path,
            owner: owner.filter(|o| !o.is_empty()),
            name,
            scope: Scope::Unscoped,
            discriminator: None,
        }
    }

    /// A declaration that the callable with anchor `parent_anchor` declares.
    /// It has no owner (invariant I1), and its kind does not keep overloads
    /// (invariant I3).
    pub fn scoped(
        kind: NodeKind,
        path: &'a str,
        name: &'a str,
        parent_anchor: u64,
    ) -> Result<Self, IdentityError> {
        check_path(path)?;
        if uid::keeps_overloads(kind) {
            return Err(IdentityError::ScopedOverloadKind { kind, decl: None });
        }
        Ok(Self::scoped_unchecked(kind, path, name, parent_anchor))
    }

    /// [`DeclKey::scoped`] for a path and a kind the caller has already
    /// checked.
    fn scoped_unchecked(kind: NodeKind, path: &'a str, name: &'a str, parent_anchor: u64) -> Self {
        Self {
            kind,
            path,
            owner: None,
            name,
            scope: Scope::Lexical { parent_anchor },
            discriminator: None,
        }
    }

    pub fn with_discriminator(self, d: Discriminator<'a>) -> Result<Self, IdentityError> {
        match d.text().is_empty() {
            true => Err(IdentityError::EmptyDiscriminator),
            false => Ok(Self {
                discriminator: Some(d),
                ..self
            }),
        }
    }

    pub fn kind(&self) -> NodeKind {
        self.kind
    }
    pub fn path(&self) -> &'a str {
        self.path
    }
    pub fn owner(&self) -> Option<&'a str> {
        self.owner
    }
    pub fn name(&self) -> &'a str {
        self.name
    }
    pub fn scope(&self) -> Scope {
        self.scope
    }
    pub fn discriminator(&self) -> Option<Discriminator<'a>> {
        self.discriminator
    }

    /// The base uid, before overload numbering ([`UidSlot`]). For an
    /// unscoped key it equals `uid::compute(kind, path, owner, name)`.
    pub fn uid(&self) -> u64 {
        match self.scope {
            Scope::Unscoped => uid::compute(self.kind, self.path, self.owner, self.name),
            Scope::Lexical { parent_anchor } => scoped_uid(parent_anchor, self.kind, self.name),
        }
    }

    /// The semantic identity: the base uid plus the discriminator. Equal to
    /// [`DeclKey::uid`] when there is no discriminator. Overlay pairing,
    /// `diff` and a scoped child's uid read the anchor, never an ordinal.
    pub fn anchor(&self) -> u64 {
        anchor_of(self.uid(), self.discriminator)
    }
}

/// The anchor of a declaration whose base uid is `base`: `base` itself
/// without a discriminator, so a caller that holds the uid hashes once.
fn anchor_of(base: u64, discriminator: Option<Discriminator<'_>>) -> u64 {
    let Some(d) = discriminator else {
        return base;
    };
    let mut h = Xxh3::new();
    h.update(&[ANCHOR_TAG]);
    h.update(&base.to_le_bytes());
    h.update(&[d.stored_tag(), 0]);
    h.update(d.text().as_bytes());
    h.update(&[0xFF]);
    h.digest()
}

/// The uid of a lexically scoped declaration.
pub fn scoped_uid(parent_anchor: u64, kind: NodeKind, name: &str) -> u64 {
    let mut h = Xxh3::new();
    h.update(&[SCOPED_TAG]);
    h.update(&parent_anchor.to_le_bytes());
    h.update(kind.as_str().as_bytes());
    h.update(b"\0");
    h.update(name.as_bytes());
    h.digest()
}

/// The stored uid of one declaration of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UidSlot {
    /// No earlier declaration of the file has this base uid.
    First(u64),
    /// A later declaration with an equal base uid, kept as its own node:
    /// `uid::overload(base, ordinal)`, `ordinal` 1-based among the later
    /// ones. Applies to kinds that keep overloads, to scoped nodes, and to a
    /// declaration whose arity path ([`FileDecl::arity`]) no earlier
    /// declaration of the base uid has (C# `Conv<T>` after `Conv`,
    /// `Foo1<T>.foo` after `Foo1.foo`).
    Later { first: u32, ordinal: u32, uid: u64 },
    /// A later declaration of another kind with an equal base uid. The index
    /// tombstones it today; `uid` is the base uid it repeats. `first` is the
    /// first declaration of the base uid, or, for a declaration with an
    /// arity, the first one of the same arity path: the twin that survives
    /// it.
    Duplicate { first: u32, uid: u64 },
}

impl UidSlot {
    pub fn uid(self) -> u64 {
        match self {
            Self::First(uid) | Self::Later { uid, .. } | Self::Duplicate { uid, .. } => uid,
        }
    }
}

/// Whether a declaration's anchor identifies it inside its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uniqueness {
    /// No other declaration of the file has this anchor.
    Unique,
    /// `count` (at least 2) declarations share the anchor; this one is at
    /// `position` (0-based) in source order. A position proves nothing about
    /// which one an edit kept, so it never pairs and never targets alone.
    Shared { position: u32, count: u32 },
}

/// One declaration of a file, as the identity rule reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDecl<'a> {
    pub kind: NodeKind,
    pub owner: Option<&'a str>,
    pub name: &'a str,
    /// Index of the lexical parent in the same slice, from [`lexical_parents`].
    pub parent: Option<u32>,
    pub discriminator: Option<Discriminator<'a>>,
    /// The declared generic arity of a declaration whose language keeps two
    /// same-named types of different arity apart (C#: a type's or a
    /// delegate's arity, see `analyzer::lexical::splits_by_arity`, and 0
    /// for every other declaration); `None` everywhere else. A later
    /// unscoped declaration of an equal base uid repeats an earlier one only
    /// when both have one arity path (this arity, then that of each
    /// [`Self::arity_scope`] outward): `Conv<T>` after `Conv`, and
    /// `Foo1<T>.foo` after `Foo1.foo`, are a [`UidSlot::Later`]; an equal
    /// path is still a [`UidSlot::Duplicate`]. The arity enters neither the
    /// base uid nor the anchor, so both declarations share one anchor.
    pub arity: Option<u32>,
    /// Index, in the same slice, of the innermost type that encloses this
    /// declaration, where [`Self::arity`] is set: the next step of the
    /// arity path. `None` at the top level, wherever the arity is `None`,
    /// and in a file where no two types share a name, since no two paths
    /// of one base uid can differ there.
    pub arity_scope: Option<u32>,
}

/// Whether declarations `a` and `b` have one arity path: an equal
/// [`FileDecl::arity`], then an equal one for each enclosing
/// [`FileDecl::arity_scope`] outward, until both reach the top level or one
/// enclosing type. A walk longer than the slice is a cycle and has no path.
fn same_arity_path(decls: &[FileDecl<'_>], mut a: usize, mut b: usize) -> bool {
    for _ in 0..=decls.len() {
        if a == b {
            return true;
        }
        if decls[a].arity != decls[b].arity {
            return false;
        }
        match (decls[a].arity_scope, decls[b].arity_scope) {
            (None, None) => return true,
            (Some(x), Some(y)) => (a, b) = (x as usize, y as usize),
            _ => return false,
        }
    }
    false
}

/// The identity of one declaration of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclIdentity {
    pub slot: UidSlot,
    pub anchor: u64,
    pub uniqueness: Uniqueness,
}

/// Every identity of one file's declarations, in slice order. A parent may
/// come before or after its children in the slice. A non-canonical `path`
/// is rejected even when `decls` is empty.
pub fn assign_file(path: &str, decls: &[FileDecl<'_>]) -> Result<Vec<DeclIdentity>, IdentityError> {
    check_path(path)?;
    let n = decls.len();
    if let Some(decl) = decls
        .iter()
        .position(|d| d.arity_scope.is_some_and(|s| s as usize >= n))
    {
        return Err(IdentityError::BadParent { decl: decl as u32 });
    }
    let mut anchors: Vec<Option<u64>> = vec![None; n];
    let mut base_uids = vec![0u64; n];
    let mut chain: Vec<usize> = Vec::new();
    for start in 0..n {
        chain.clear();
        let mut i = start;
        loop {
            if anchors[i].is_some() {
                break;
            }
            if chain.len() > n {
                return Err(IdentityError::BadParent { decl: i as u32 });
            }
            chain.push(i);
            match decls[i].parent {
                None => break,
                Some(p) if (p as usize) < n => i = p as usize,
                Some(_) => return Err(IdentityError::BadParent { decl: i as u32 }),
            }
        }
        for &j in chain.iter().rev() {
            let d = &decls[j];
            let key = match d.parent {
                None => DeclKey::unscoped_unchecked(d.kind, path, d.owner, d.name),
                Some(p) => {
                    if d.owner.is_some_and(|o| !o.is_empty()) {
                        return Err(IdentityError::ScopedWithOwner { decl: j as u32 });
                    }
                    if uid::keeps_overloads(d.kind) {
                        return Err(IdentityError::ScopedOverloadKind {
                            kind: d.kind,
                            decl: Some(j as u32),
                        });
                    }
                    let parent_anchor =
                        anchors[p as usize].expect("the chain resolves a parent before its child");
                    DeclKey::scoped_unchecked(d.kind, path, d.name, parent_anchor)
                }
            };
            let key = match d.discriminator {
                Some(x) => key.with_discriminator(x)?,
                None => key,
            };
            let base = key.uid();
            base_uids[j] = base;
            anchors[j] = Some(anchor_of(base, key.discriminator));
        }
    }

    let mut first_of: FxHashMap<u64, (u32, u32)> =
        FxHashMap::with_capacity_and_hasher(n, Default::default());
    let mut anchor_count: FxHashMap<u64, u32> =
        FxHashMap::with_capacity_and_hasher(n, Default::default());
    for a in anchors.iter().flatten() {
        *anchor_count.entry(*a).or_insert(0) += 1;
    }
    let mut anchor_seen: FxHashMap<u64, u32> = FxHashMap::default();
    let mut out = Vec::with_capacity(n);
    for (j, d) in decls.iter().enumerate() {
        let base = base_uids[j];
        let slot = match first_of.entry(base) {
            Entry::Vacant(slot) => {
                slot.insert((j as u32, 0));
                UidSlot::First(base)
            }
            Entry::Occupied(mut slot) => {
                let (first, later) = slot.get_mut();
                // The earlier declaration this one repeats. An overload or a
                // scoped node repeats none. A declaration with an arity
                // repeats the first one of its arity path, and is a
                // declaration of its own when no earlier one has that path.
                let twin = match (uid::keeps_overloads(d.kind) || d.parent.is_some(), d.arity) {
                    (true, _) => None,
                    (false, Some(_)) => {
                        (0..j).find(|&k| base_uids[k] == base && same_arity_path(decls, k, j))
                    }
                    (false, None) => Some(*first as usize),
                };
                match twin {
                    None => {
                        *later += 1;
                        UidSlot::Later {
                            first: *first,
                            ordinal: *later,
                            uid: uid::overload(base, *later),
                        }
                    }
                    Some(twin) => UidSlot::Duplicate {
                        first: twin as u32,
                        uid: base,
                    },
                }
            }
        };
        let anchor = anchors[j].expect("every declaration is resolved");
        let count = anchor_count[&anchor];
        let uniqueness = match count {
            1 => Uniqueness::Unique,
            _ => {
                let seen = anchor_seen.entry(anchor).or_insert(0);
                let position = *seen;
                *seen += 1;
                Uniqueness::Shared { position, count }
            }
        };
        out.push(DeclIdentity {
            slot,
            anchor,
            uniqueness,
        });
    }
    Ok(out)
}

/// For each dirty declaration, the base declaration it replaces. A pair
/// forms only when the anchor occurs exactly once among the base
/// declarations and exactly once among the dirty ones of the same file.
pub fn pair_by_anchor(base: &[u64], dirty: &[u64]) -> Vec<Option<u32>> {
    let mut base_at: FxHashMap<u64, Option<u32>> = FxHashMap::default();
    for (i, &a) in base.iter().enumerate() {
        base_at
            .entry(a)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(i as u32));
    }
    let mut dirty_count: FxHashMap<u64, u32> = FxHashMap::default();
    for &a in dirty {
        *dirty_count.entry(a).or_insert(0) += 1;
    }
    dirty
        .iter()
        .map(|a| match dirty_count[a] {
            1 => base_at.get(a).copied().flatten(),
            _ => None,
        })
        .collect()
}

// ── Lexical parent ──────────────────────────────────────────────────────

/// The kinds whose span bounds a lexical scope (design A §3.1).
pub const fn is_scope_container(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Constructor
            | NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Impl
            | NodeKind::Namespace
            | NodeKind::Module
    )
}

/// The lexical parent of each declaration of one file: the index of the
/// innermost scope container that strictly contains it, when that container
/// is a callable and the declaration's kind is in `scoped_kinds` and does not
/// keep overloads. A class or module between the two breaks the link.
/// `scoped_kinds` is the language's opt-in list; empty means no node is
/// scoped.
pub fn lexical_parents(decls: &[(NodeKind, Span)], scoped_kinds: &[NodeKind]) -> Vec<Option<u32>> {
    if scoped_kinds.is_empty() {
        return vec![None; decls.len()];
    }
    let spans: Vec<Span> = decls.iter().map(|&(_, span)| span).collect();
    let enclosing = innermost_enclosing(&spans);
    decls
        .iter()
        .enumerate()
        .map(|(i, &(kind, _))| {
            if !scoped_kinds.contains(&kind) || uid::keeps_overloads(kind) {
                return None;
            }
            let mut up = enclosing[i];
            while let Some(j) = up {
                if is_scope_container(decls[j].0) {
                    break;
                }
                up = enclosing[j];
            }
            up.filter(|&j| decls[j].0.is_callable()).map(|j| j as u32)
        })
        .collect()
}

// ── Target request ──────────────────────────────────────────────────────

/// One qualifier segment of a target request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// An owner, module or lexical parent name in [`owner_match_form`]. It
    /// matches a path segment whose match form is equal.
    Name(String),
    /// `` `text` ``: the exact text of a path segment, such as a closure
    /// parent `<anonymous:3:4>` or the owner `Walk<'s, 't>`. Inside the
    /// backticks, a doubled backtick is one backtick.
    Literal(String),
    /// `<A as Display>`: the type `A` in the `impl Display for A` block.
    ImplTrait { self_ty: String, trait_name: String },
}

/// The part of a target request before the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Qualifier {
    /// A bare name: every declaration with that name.
    Any,
    /// `::name` or `.name`: only a declaration with no owner and no lexical
    /// parent.
    Root,
    /// `a.b.name` / `a::b::name`: a declaration whose qualified path ends
    /// with these segments.
    Path(Vec<Segment>),
}

/// Why a target text does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSyntaxError {
    Empty,
    EmptySegment,
    UnclosedAngle,
    /// A `` ` `` opens a literal segment that no `` ` `` closes.
    UnclosedQuote,
    /// `<T as Tr>` must qualify a name; it cannot be the name.
    ImplTraitAsName,
    /// `<…>` without the form `<Type as Trait>`.
    BadImplTrait(String),
    /// A leading separator marks a root declaration and takes a bare name.
    RootWithQualifier,
    /// `@` followed by digits that are not a line number from 1.
    BadLine(String),
    UnknownKind(String),
}

impl fmt::Display for TargetSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("target is empty"),
            Self::EmptySegment => f.write_str("target has an empty segment"),
            Self::UnclosedAngle => f.write_str("target has an unclosed `<`"),
            Self::UnclosedQuote => f.write_str("target has an unclosed backtick"),
            Self::ImplTraitAsName => f.write_str("`<Type as Trait>` must be followed by a name"),
            Self::BadImplTrait(s) => write!(f, "expected `<Type as Trait>`, got {s:?}"),
            Self::RootWithQualifier => f.write_str("a leading `::` takes a bare name"),
            Self::BadLine(s) => write!(f, "bad line number {s:?}"),
            Self::UnknownKind(k) => write!(f, "unknown kind {k:?}"),
        }
    }
}

impl std::error::Error for TargetSyntaxError {}

/// The one target syntax of every graph command (grammar in the design
/// note). Construct it with [`TargetRequest::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRequest {
    qualifier: Qualifier,
    name: String,
    line: Option<u32>,
    kind: Option<NodeKind>,
    file: Option<String>,
}

/// One segment of a candidate's qualified path: an owner segment or a
/// lexical parent name, outermost first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSegment<'a> {
    pub name: &'a str,
    /// The trait of the `impl` block whose self type this segment names.
    pub impl_trait: Option<&'a str>,
}

impl<'a> PathSegment<'a> {
    /// The segments of a stored owner, split on `.` and `::` outside a
    /// generic argument list: `a::Conv<b::C>` gives `a`, `Conv<b::C>`.
    pub fn from_owner(owner: &'a str) -> Vec<PathSegment<'a>> {
        owner_segments(owner)
            .into_iter()
            .map(|name| PathSegment {
                name,
                impl_trait: None,
            })
            .collect()
    }
}

fn owner_segments(owner: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = owner;
    while !rest.is_empty() {
        let (seg, tail) = rest.split_at(next_separator(rest));
        if !seg.is_empty() {
            out.push(seg);
        }
        rest = tail
            .strip_prefix("::")
            .or_else(|| tail.strip_prefix('.'))
            .unwrap_or(tail);
    }
    out
}

fn last_segment(s: &str) -> &str {
    owner_segments(s).last().copied().unwrap_or(s)
}

/// Whether a segment or a name must be written in backticks to parse back
/// to the same text.
fn needs_quote(s: &str) -> bool {
    s.is_empty()
        || s.starts_with(char::is_whitespace)
        || s.ends_with(char::is_whitespace)
        || s.contains(['.', ':', '<', '>', '@', '`'])
}

fn push_quoted(out: &mut String, s: &str) {
    match needs_quote(s) {
        true => push_quoted_always(out, s),
        false => out.push_str(s),
    }
}

fn push_quoted_always(out: &mut String, s: &str) {
    out.push('`');
    out.push_str(&s.replace('`', "``"));
    out.push('`');
}

/// A request's trait text against a path segment's trait text. Both
/// compare by their last segment. A request trait with generic arguments
/// compares the full text, whitespace aside, so `Conv<u8>` never matches
/// `Conv<u16>`; a request trait without them matches every instance.
fn trait_matches(want: &str, have: &str) -> bool {
    let (want, have) = (last_segment(want), last_segment(have));
    match want.contains('<') {
        true => {
            let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
            squash(want) == squash(have)
        }
        false => owner_match_form(have) == want,
    }
}

/// What the resolver knows of one declaration that has the requested name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFacts<'a> {
    /// Merged-space node index (base graph plus overlay).
    pub node: u32,
    pub kind: NodeKind,
    /// Repo-relative, `/`-separated.
    pub file: &'a str,
    /// 1-based start line, as every command prints it.
    pub line: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 0-based start and end columns.
    pub columns: (u32, u32),
    pub name: &'a str,
    /// Owner segments, then lexical parent names, outermost first. Empty
    /// for a root declaration.
    pub qualifier: Vec<PathSegment<'a>>,
}

/// How well a `--file` needle matches a path. A resolver keeps only the
/// candidates of the best tier present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileMatch {
    Substring,
    /// The path ends with `/needle`.
    Suffix,
    Equal,
}

/// The tier at which `needle` (already normalised) matches `path`.
pub fn file_match(needle: &str, path: &str) -> Option<FileMatch> {
    if !path.contains(needle) {
        return None;
    }
    let bare = needle.trim_end_matches('/');
    if bare.is_empty() {
        return Some(FileMatch::Substring);
    }
    if path == bare {
        return Some(FileMatch::Equal);
    }
    match path
        .strip_suffix(bare)
        .is_some_and(|head| head.ends_with('/'))
    {
        true => Some(FileMatch::Suffix),
        false => Some(FileMatch::Substring),
    }
}

/// A `--file` needle as the graph spells paths: `/`-separated, no `./`.
pub fn normalize_file_needle(needle: &str) -> String {
    let needle = needle.replace('\\', "/");
    needle.strip_prefix("./").unwrap_or(&needle).to_string()
}

fn split_line(text: &str) -> Result<(&str, Option<u32>), TargetSyntaxError> {
    let Some((head, tail)) = text.rsplit_once('@') else {
        return Ok((text, None));
    };
    // An odd backtick count before the `@` puts it inside a literal segment.
    let inside_quote = head.bytes().filter(|&b| b == b'`').count() % 2 == 1;
    if inside_quote
        || head.is_empty()
        || tail.is_empty()
        || !tail.bytes().all(|b| b.is_ascii_digit())
    {
        return Ok((text, None));
    }
    match tail.parse::<u32>() {
        Ok(line) if line > 0 => Ok((head, Some(line))),
        _ => Err(TargetSyntaxError::BadLine(tail.to_string())),
    }
}

/// The first `.` or `::` outside a generic argument list, so `Foo<a::B>.x`
/// splits after `Foo<a::B>`.
fn next_separator(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut depth = 0u32;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b'.' if depth == 0 => return i,
            b':' if depth == 0 && bytes.get(i + 1) == Some(&b':') => return i,
            _ => {}
        }
    }
    bytes.len()
}

fn matching_angle(s: &str) -> Option<usize> {
    let mut depth = 0u32;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_segment(seg: &str) -> Result<Segment, TargetSyntaxError> {
    let seg = seg.trim();
    if seg.is_empty() {
        return Err(TargetSyntaxError::EmptySegment);
    }
    let Some(inner) = seg.strip_prefix('<').and_then(|s| s.strip_suffix('>')) else {
        return Ok(Segment::Name(seg.to_string()));
    };
    match inner.split_once(" as ") {
        Some((ty, tr)) if !ty.trim().is_empty() && !tr.trim().is_empty() => {
            Ok(Segment::ImplTrait {
                self_ty: ty.trim().to_string(),
                trait_name: tr.trim().to_string(),
            })
        }
        _ => Err(TargetSyntaxError::BadImplTrait(seg.to_string())),
    }
}

/// The text of the literal segment that opens `s` (which starts with a
/// backtick), and the rest after its closing backtick. A doubled backtick
/// inside is one backtick.
fn read_quoted(s: &str) -> Result<(String, &str), TargetSyntaxError> {
    let mut text = String::new();
    let mut chars = s[1..].char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '`' {
            text.push(c);
            continue;
        }
        if chars.peek().is_some_and(|&(_, next)| next == '`') {
            chars.next();
            text.push('`');
            continue;
        }
        return match text.is_empty() {
            true => Err(TargetSyntaxError::EmptySegment),
            false => Ok((text, &s[1 + i + 1..])),
        };
    }
    Err(TargetSyntaxError::UnclosedQuote)
}

fn split_segments(body: &str) -> Result<Vec<Segment>, TargetSyntaxError> {
    if body.trim().is_empty() {
        return Err(TargetSyntaxError::Empty);
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let (seg, tail) = match rest.as_bytes().first() {
            Some(b'`') => {
                let (literal, tail) = read_quoted(rest)?;
                out.push(Segment::Literal(literal));
                (&rest[..rest.len() - tail.len()], tail)
            }
            Some(b'<') => {
                let close = matching_angle(rest).ok_or(TargetSyntaxError::UnclosedAngle)?;
                let (seg, tail) = (&rest[..=close], &rest[close + 1..]);
                out.push(parse_segment(seg)?);
                (seg, tail)
            }
            _ => {
                let (seg, tail) = rest.split_at(next_separator(rest));
                out.push(parse_segment(seg)?);
                (seg, tail)
            }
        };
        if tail.is_empty() {
            return Ok(out);
        }
        rest = tail
            .strip_prefix("::")
            .or_else(|| tail.strip_prefix('.'))
            .ok_or_else(|| TargetSyntaxError::BadImplTrait(seg.to_string()))?;
        if rest.is_empty() {
            return Err(TargetSyntaxError::EmptySegment);
        }
    }
}

impl Segment {
    fn matches(&self, have: &PathSegment<'_>) -> bool {
        match self {
            Self::Name(n) => owner_match_form(have.name) == n.as_str(),
            Self::Literal(text) => text == have.name,
            Self::ImplTrait {
                self_ty,
                trait_name,
            } => {
                owner_match_form(last_segment(self_ty)) == owner_match_form(have.name)
                    && have
                        .impl_trait
                        .is_some_and(|t| trait_matches(trait_name, t))
            }
        }
    }

    fn render(&self, out: &mut String) {
        match self {
            Self::Name(n) => out.push_str(n),
            Self::Literal(text) => push_quoted_always(out, text),
            Self::ImplTrait {
                self_ty,
                trait_name,
            } => {
                out.push('<');
                out.push_str(self_ty);
                out.push_str(" as ");
                out.push_str(trait_name);
                out.push('>');
            }
        }
    }
}

impl Qualifier {
    /// Whether a candidate with this qualified path satisfies the qualifier.
    /// A path qualifier matches a segment-bounded suffix of the path.
    pub fn matches(&self, path: &[PathSegment<'_>]) -> bool {
        match self {
            Self::Any => true,
            Self::Root => path.is_empty(),
            Self::Path(want) => {
                want.len() <= path.len()
                    && want
                        .iter()
                        .zip(&path[path.len() - want.len()..])
                        .all(|(w, have)| w.matches(have))
            }
        }
    }

    fn of_path(path: &[PathSegment<'_>]) -> Self {
        match path.is_empty() {
            true => Self::Root,
            false => Self::Path(
                path.iter()
                    .map(|s| match s.impl_trait {
                        Some(t) => Segment::ImplTrait {
                            self_ty: s.name.to_string(),
                            trait_name: t.to_string(),
                        },
                        None if needs_quote(s.name) || owner_match_form(s.name) != s.name => {
                            Segment::Literal(s.name.to_string())
                        }
                        None => Segment::Name(s.name.to_string()),
                    })
                    .collect(),
            ),
        }
    }
}

impl TargetRequest {
    /// Parse the target text and the `--kind` and `--file` filters.
    pub fn parse(
        text: &str,
        kind: Option<&str>,
        file: Option<&str>,
    ) -> Result<Self, TargetSyntaxError> {
        let (body, line) = split_line(text.trim())?;
        let (root, body) = match body.strip_prefix("::").or_else(|| body.strip_prefix('.')) {
            Some(rest) => (true, rest),
            None => (false, body),
        };
        let mut segments = split_segments(body)?;
        let name = match segments.pop() {
            Some(Segment::Name(n) | Segment::Literal(n)) => n,
            Some(Segment::ImplTrait { .. }) => return Err(TargetSyntaxError::ImplTraitAsName),
            None => return Err(TargetSyntaxError::Empty),
        };
        for seg in &mut segments {
            if let Segment::Name(n) = seg {
                let match_form = owner_match_form(n).into_owned();
                *n = match_form;
            }
        }
        let qualifier = match (root, segments.is_empty()) {
            (true, true) => Qualifier::Root,
            (true, false) => return Err(TargetSyntaxError::RootWithQualifier),
            (false, true) => Qualifier::Any,
            (false, false) => Qualifier::Path(segments),
        };
        let kind = kind
            .map(|k| {
                k.parse::<NodeKind>()
                    .map_err(|()| TargetSyntaxError::UnknownKind(k.to_string()))
            })
            .transpose()?;
        let file = file.map(normalize_file_needle).filter(|f| !f.is_empty());
        Ok(Self {
            qualifier,
            name,
            line,
            kind,
            file,
        })
    }

    pub fn qualifier(&self) -> &Qualifier {
        &self.qualifier
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn line(&self) -> Option<u32> {
        self.line
    }
    pub fn kind(&self) -> Option<NodeKind> {
        self.kind
    }
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// The target text in canonical form: segments joined by `.`, a root
    /// declaration as `::name`, the line as `@N`. A literal segment, and a
    /// name that holds `.`, `:`, `<`, `>`, `@` or a backtick, go in
    /// backticks. [`TargetRequest::parse`] reads it back to an equal
    /// qualifier, name and line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        match &self.qualifier {
            Qualifier::Any => {}
            Qualifier::Root => out.push_str("::"),
            Qualifier::Path(segments) => {
                for seg in segments {
                    seg.render(&mut out);
                    out.push('.');
                }
            }
        }
        push_quoted(&mut out, &self.name);
        if let Some(line) = self.line {
            out.push('@');
            out.push_str(&line.to_string());
        }
        out
    }

    /// Whether one candidate passes the name, kind, qualifier and line
    /// filters. The file filter is a set-level rule: see [`select`].
    fn admits(&self, c: &CandidateFacts<'_>) -> bool {
        c.name == self.name
            && self.kind.is_none_or(|k| k == c.kind)
            && self.line.is_none_or(|l| l == c.line)
            && self.qualifier.matches(&c.qualifier)
    }
}

/// The candidates `request` resolves to, as indices into `candidates`: the
/// per-candidate filters, then the best `--file` tier present, then the
/// constructor exception of [`drop_owned_constructors`].
pub fn select(request: &TargetRequest, candidates: &[CandidateFacts<'_>]) -> Vec<usize> {
    let admitted = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| request.admits(c));
    let picked: Vec<usize> = match request.file() {
        None => admitted.map(|(i, _)| i).collect(),
        Some(needle) => {
            let ranked: Vec<(usize, FileMatch)> = admitted
                .filter_map(|(i, c)| file_match(needle, c.file).map(|m| (i, m)))
                .collect();
            let best = ranked.iter().map(|&(_, m)| m).max();
            ranked
                .into_iter()
                .filter(|&(_, m)| Some(m) == best)
                .map(|(i, _)| i)
                .collect()
        }
    };
    drop_owned_constructors(request, candidates, picked)
}

/// A bare type name (`Foo`) selects the type over its own same-named
/// constructors (`class Foo { Foo() {} }` in C#, Java, C++, Dart). A
/// constructor drops only when its owner is exactly that name and exactly
/// one type of the same name encloses its whole span, columns included:
/// the stored owner of a constructor is the bare class name and never the
/// enclosing path, so the enclosing declaration is the proof. A definition
/// outside its class (C++ `Foo::Foo() {}`, even on the class's line) sits
/// in no class's span and stays; a Kotlin or C# primary constructor sits
/// inside its class's span on the class's line. A qualifier, `--kind` or
/// `@line` in the request says the caller wants the constructor to compete.
fn drop_owned_constructors(
    request: &TargetRequest,
    candidates: &[CandidateFacts<'_>],
    picked: Vec<usize>,
) -> Vec<usize> {
    if request.qualifier != Qualifier::Any || request.kind.is_some() || request.line.is_some() {
        return picked;
    }
    let owned = |c: &CandidateFacts<'_>| {
        c.kind == NodeKind::Constructor
            && matches!(c.qualifier.as_slice(), [owner] if owner.name == c.name && owner.impl_trait.is_none())
            && {
                let mut types = picked.iter().map(|&i| &candidates[i]).filter(|t| {
                    t.kind.is_type() && t.name == c.name && t.file == c.file && encloses(t, c)
                });
                types.next().is_some() && types.next().is_none()
            }
    };
    picked
        .iter()
        .copied()
        .filter(|&i| !owned(&candidates[i]))
        .collect()
}

/// `outer`'s span holds `inner`'s, compared as (line, column) pairs.
fn encloses(outer: &CandidateFacts<'_>, inner: &CandidateFacts<'_>) -> bool {
    (outer.line, outer.columns.0) <= (inner.line, inner.columns.0)
        && (inner.end_line, inner.columns.1) <= (outer.end_line, outer.columns.1)
}

/// One declaration a request named, as the answer reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub node: u32,
    pub kind: NodeKind,
    pub file: String,
    pub line: u32,
    /// The qualified target text, without file, kind or line.
    pub display: String,
    /// The candidate has an owner or a lexical parent.
    pub qualified: bool,
    /// A request that resolves to this candidate alone among the
    /// candidates of the same answer. `None` when no target text, file, line
    /// and kind tell it apart.
    pub reusable: Option<TargetRequest>,
}

impl Candidate {
    fn from_facts(c: &CandidateFacts<'_>) -> Self {
        let base = TargetRequest {
            qualifier: Qualifier::of_path(&c.qualifier),
            name: c.name.to_string(),
            line: None,
            kind: None,
            file: None,
        };
        Self {
            node: c.node,
            kind: c.kind,
            file: c.file.to_string(),
            line: c.line,
            display: base.render(),
            qualified: !c.qualifier.is_empty(),
            reusable: None,
        }
    }
}

/// Two or more declarations match one request. The answer lists them all;
/// the command never picks one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ambiguity {
    candidates: Vec<Candidate>,
}

impl Ambiguity {
    /// `picked` indexes the matching declarations in `named`. Each
    /// candidate's reusable request escalates from the qualified name
    /// through `--file`, `@line` and `--kind` until it selects that
    /// candidate alone among all of `named`. Cost: O(k·n) for k picked of n
    /// named, on the error path only.
    fn new(named: &[CandidateFacts<'_>], picked: &[usize]) -> Self {
        let candidates = picked
            .iter()
            .map(|&i| {
                let c = &named[i];
                let mut out = Candidate::from_facts(c);
                let mut req = TargetRequest {
                    qualifier: Qualifier::of_path(&c.qualifier),
                    name: c.name.to_string(),
                    line: None,
                    kind: None,
                    file: None,
                };
                for step in 0..4 {
                    match step {
                        0 => {}
                        1 => req.file = Some(c.file.to_string()),
                        2 => req.line = Some(c.line),
                        _ => req.kind = Some(c.kind),
                    }
                    if select(&req, named) == [i] {
                        out.reusable = Some(req.clone());
                        break;
                    }
                }
                out
            })
            .collect();
        Self { candidates }
    }

    /// The true number of matching declarations.
    pub fn total(&self) -> usize {
        self.candidates.len()
    }

    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// Today's error, with the same message: `EcpError::AmbiguousSymbol`.
    pub fn to_error(&self, request: &TargetRequest) -> EcpError {
        let lines = self
            .candidates
            .iter()
            .map(|c| format!("  {},{},{}", c.file, c.kind.as_str(), c.line))
            .collect::<Vec<_>>()
            .join("\n");
        EcpError::AmbiguousSymbol {
            name: request.render(),
            count: self.total(),
            candidates: Some(lines),
            file_given: request.file.is_some(),
            kind_given: request.kind.is_some(),
            owner_form: request.qualifier == Qualifier::Any
                && self.candidates.iter().any(|c| c.qualified),
        }
    }
}

/// The one result of every name-to-node lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetResolution {
    Unique(Candidate),
    Ambiguous(Ambiguity),
    /// No declaration passes the filters. `named` counts the declarations
    /// with the requested name before the filters: 0 means the name is not
    /// declared, more means a filter excluded every one.
    NotFound {
        named: u32,
    },
}

/// Resolve `request` against every declaration that has the requested name
/// (the caller reads them from the merged graph's name index, with deleted
/// declarations already removed).
pub fn resolve_target(request: &TargetRequest, named: &[CandidateFacts<'_>]) -> TargetResolution {
    let picked = select(request, named);
    match picked.as_slice() {
        [] => TargetResolution::NotFound {
            named: named.iter().filter(|c| c.name == request.name).count() as u32,
        },
        [one] => TargetResolution::Unique(Candidate::from_facts(&named[*one])),
        many => TargetResolution::Ambiguous(Ambiguity::new(named, many)),
    }
}

// ── Route and handler identity ──────────────────────────────────────────

/// Why a route key cannot be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteKeyError {
    NonCanonicalFile(String),
    EmptyVerb,
    /// The stored name is `"<VERB> <path>"`, split on the first space.
    VerbWithWhitespace(String),
    EmptyPath,
}

impl fmt::Display for RouteKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonCanonicalFile(p) => write!(f, "route file is not canonical: {p:?}"),
            Self::EmptyVerb => f.write_str("route verb is empty"),
            Self::VerbWithWhitespace(v) => write!(f, "route verb holds whitespace: {v:?}"),
            Self::EmptyPath => f.write_str("route path is empty"),
        }
    }
}

impl std::error::Error for RouteKeyError {}

/// The identity of one endpoint: the registering file, the verb and the
/// full path with every prefix composed. It never holds the handler.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteKey {
    file: String,
    verb: String,
    path: String,
}

impl RouteKey {
    /// `verb` is upper-cased (`get` and `GET` are one verb); `path` is
    /// trimmed and must already hold its composed prefixes
    /// ([`join_route_path`]).
    pub fn new(file: &str, verb: &str, path: &str) -> Result<Self, RouteKeyError> {
        if check_path(file).is_err() {
            return Err(RouteKeyError::NonCanonicalFile(file.to_string()));
        }
        let verb = verb.trim();
        if verb.is_empty() {
            return Err(RouteKeyError::EmptyVerb);
        }
        if verb.chars().any(char::is_whitespace) {
            return Err(RouteKeyError::VerbWithWhitespace(verb.to_string()));
        }
        let path = path.trim();
        if path.is_empty() {
            return Err(RouteKeyError::EmptyPath);
        }
        Ok(Self {
            file: file.to_string(),
            verb: verb.to_ascii_uppercase(),
            path: path.to_string(),
        })
    }

    pub fn file(&self) -> &str {
        &self.file
    }
    pub fn verb(&self) -> &str {
        &self.verb
    }
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The stored `Route` node name, `"<VERB> <path>"`, as today.
    pub fn name(&self) -> String {
        format!("{} {}", self.verb, self.path)
    }

    /// The `Route` node uid: today's `uid::compute(Route, file, None, name)`.
    pub fn uid(&self) -> u64 {
        uid::compute(NodeKind::Route, &self.file, None, &self.name())
    }

    /// The inverse of [`RouteKey::name`]: `(verb, path)`.
    pub fn split_name(name: &str) -> Option<(&str, &str)> {
        name.split_once(' ')
            .filter(|(verb, path)| !verb.is_empty() && !path.is_empty())
    }
}

/// Compose a router prefix and a route path with exactly one `/` at the
/// joint and a leading `/`. An empty route path gives the prefix as
/// written; a slash-only route path gives the prefix with one trailing `/`,
/// so `("/api", "")` is `/api` and `("/api", "/")` is `/api/`. A trailing
/// `/` of a longer route path is kept.
pub fn join_route_path(prefix: &str, path: &str) -> String {
    let (prefix, path) = (prefix.trim(), path.trim());
    let joined = match path.is_empty() {
        true => prefix.to_string(),
        false => format!(
            "{}/{}",
            prefix.trim_end_matches('/'),
            path.trim_start_matches('/')
        ),
    };
    match joined.starts_with('/') {
        true => joined,
        false => format!("/{joined}"),
    }
}

/// What serves a route. Its own key, never part of the [`RouteKey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerRef {
    /// A declaration node; `uid` is its [`UidSlot::uid`].
    Declaration { uid: u64 },
    /// An anonymous function written at the registration that has its own
    /// closure node (a name that starts with `<anonymous:`). A parser emits
    /// that node only when the body holds a call. `uid` is the node's
    /// [`UidSlot::uid`].
    Closure { uid: u64 },
    /// An anonymous function written at the registration with no node.
    /// `line` is the registration's 1-based line.
    Inline { line: u32 },
    /// The registration names a handler that binds no declaration.
    Unresolved { name: String },
    /// The producer saw no handler.
    Absent,
}

impl HandlerRef {
    /// The `handlerKind` value of `ecp routes`.
    pub fn handler_kind(&self) -> &'static str {
        match self {
            Self::Declaration { .. } => "named",
            Self::Closure { .. } | Self::Inline { .. } => "inline_anonymous",
            Self::Unresolved { .. } => "unresolved",
            Self::Absent => "absent",
        }
    }
}

/// One `HandlesRoute` fact. A route has zero or more bindings; a handler
/// serves zero or more routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteBinding {
    pub route: RouteKey,
    pub handler: HandlerRef,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl<'a>(kind: NodeKind, owner: Option<&'a str>, name: &'a str) -> FileDecl<'a> {
        FileDecl {
            kind,
            owner,
            name,
            parent: None,
            discriminator: None,
            arity: None,
            arity_scope: None,
        }
    }

    fn facts<'a>(
        node: u32,
        kind: NodeKind,
        file: &'a str,
        line: u32,
        qualifier: Vec<PathSegment<'a>>,
        name: &'a str,
    ) -> CandidateFacts<'a> {
        CandidateFacts {
            node,
            kind,
            file,
            line,
            end_line: line,
            columns: (0, 0),
            name,
            qualifier,
        }
    }

    fn seg(name: &str) -> PathSegment<'_> {
        PathSegment {
            name,
            impl_trait: None,
        }
    }

    // ── declaration identity ──

    #[test]
    fn test_canonical_path_backslash_replaced_dot_prefix_kept() {
        assert_eq!(canonical_path("src\\a.rs"), "src/a.rs");
        assert_eq!(canonical_path(".\\a.rs"), "./a.rs");
        assert_eq!(canonical_path(".\\.\\a.rs"), "././a.rs");
        assert!(matches!(
            canonical_path("./src/a.rs"),
            Cow::Borrowed("./src/a.rs")
        ));
        assert!(matches!(canonical_path("src/a.rs"), Cow::Borrowed(_)));
    }

    #[test]
    fn test_unscoped_dot_prefix_path_accepted_and_hashed_as_written() {
        let key = DeclKey::unscoped(NodeKind::Function, "./a.rs", None, "f").unwrap();
        assert_eq!(key.path(), "./a.rs");
        assert_eq!(
            key.uid(),
            uid::compute(NodeKind::Function, "./a.rs", None, "f")
        );
        assert_ne!(
            key.uid(),
            uid::compute(NodeKind::Function, "a.rs", None, "f")
        );
    }

    #[test]
    fn test_owner_match_form_generic_arguments_removed() {
        assert_eq!(owner_match_form("Walk<'s, 't>"), "Walk");
        assert_eq!(owner_match_form("a::Foo<Vec<T>>::Bar"), "a::Foo::Bar");
        assert_eq!(owner_match_form(" Foo "), "Foo");
        assert!(matches!(owner_match_form("a::b"), Cow::Borrowed("a::b")));
        assert_eq!(owner_match_form("<anonymous:3:4>"), "<anonymous:3:4>");
    }

    #[test]
    fn test_unscoped_uid_equals_uid_compute() {
        let key = DeclKey::unscoped(NodeKind::Method, "src/a.rs", Some("A"), "fmt").unwrap();
        assert_eq!(
            key.uid(),
            uid::compute(NodeKind::Method, "src/a.rs", Some("A"), "fmt")
        );
        assert_eq!(key.anchor(), key.uid());
    }

    #[test]
    fn test_unscoped_empty_owner_equals_no_owner() {
        let empty = DeclKey::unscoped(NodeKind::Function, "a.py", Some(""), "f").unwrap();
        let none = DeclKey::unscoped(NodeKind::Function, "a.py", None, "f").unwrap();
        assert_eq!(empty, none);
        assert_eq!(empty.owner(), None);
    }

    #[test]
    fn test_unscoped_non_canonical_path_rejected() {
        assert_eq!(
            DeclKey::unscoped(NodeKind::Function, "src\\a.rs", None, "f"),
            Err(IdentityError::NonCanonicalPath("src\\a.rs".into()))
        );
    }

    #[test]
    fn test_unscoped_generic_owner_kept_as_produced() {
        let u8_key = DeclKey::unscoped(NodeKind::Impl, "a.rs", Some("Conv<u8>"), "A").unwrap();
        let u16_key = DeclKey::unscoped(NodeKind::Impl, "a.rs", Some("Conv<u16>"), "A").unwrap();
        assert_eq!(u8_key.owner(), Some("Conv<u8>"));
        assert_eq!(
            u8_key.uid(),
            uid::compute(NodeKind::Impl, "a.rs", Some("Conv<u8>"), "A")
        );
        assert_ne!(u8_key.uid(), u16_key.uid());
        let walk = DeclKey::unscoped(NodeKind::Method, "a.rs", Some("Walk<'s, 't>"), "f").unwrap();
        assert_eq!(
            walk.uid(),
            uid::compute(NodeKind::Method, "a.rs", Some("Walk<'s, 't>"), "f")
        );
    }

    #[test]
    fn test_scoped_method_kind_rejected() {
        assert_eq!(
            DeclKey::scoped(NodeKind::Method, "a.rs", "m", 7),
            Err(IdentityError::ScopedOverloadKind {
                kind: NodeKind::Method,
                decl: None
            })
        );
    }

    #[test]
    fn test_scoped_constructor_kind_rejected() {
        assert_eq!(
            DeclKey::scoped(NodeKind::Constructor, "a.rs", "new", 7),
            Err(IdentityError::ScopedOverloadKind {
                kind: NodeKind::Constructor,
                decl: None
            })
        );
    }

    #[test]
    fn test_discriminator_stored_tag_round_trip() {
        for d in [
            Discriminator::ImplTrait("Conv<u8>"),
            Discriminator::Signature("(i32, &str)"),
            Discriminator::Tag("Deployment"),
        ] {
            assert_eq!(
                Discriminator::from_stored(d.stored_tag(), d.text()),
                Some(d)
            );
        }
        assert_eq!(Discriminator::from_stored(0, "x"), None);
        assert_eq!(Discriminator::from_stored(4, "x"), None);
    }

    #[test]
    fn test_with_discriminator_empty_text_rejected() {
        let key = DeclKey::unscoped(NodeKind::Method, "a.rs", Some("A"), "fmt").unwrap();
        assert_eq!(
            key.with_discriminator(Discriminator::ImplTrait("")),
            Err(IdentityError::EmptyDiscriminator)
        );
    }

    #[test]
    fn test_anchor_discriminator_separates_equal_uids() {
        let key = DeclKey::unscoped(NodeKind::Method, "a.rs", Some("A"), "fmt").unwrap();
        let display = key
            .with_discriminator(Discriminator::ImplTrait("Display"))
            .unwrap();
        let debug = key
            .with_discriminator(Discriminator::ImplTrait("Debug"))
            .unwrap();
        assert_eq!(display.uid(), debug.uid());
        assert_ne!(display.anchor(), debug.anchor());
        assert_ne!(display.anchor(), key.anchor());
        let tag = key
            .with_discriminator(Discriminator::Tag("Display"))
            .unwrap();
        assert_ne!(tag.anchor(), display.anchor());
    }

    #[test]
    fn test_scoped_uid_differs_from_unscoped_and_hashes_parent() {
        let scoped = DeclKey::scoped(NodeKind::Function, "a.rs", "h", 7).unwrap();
        assert_eq!(scoped.owner(), None);
        assert_eq!(scoped.uid(), scoped_uid(7, NodeKind::Function, "h"));
        assert_ne!(scoped.uid(), scoped_uid(8, NodeKind::Function, "h"));
        assert_ne!(
            scoped.uid(),
            uid::compute(NodeKind::Function, "a.rs", None, "h")
        );
    }

    #[test]
    fn test_uid_slot_uid_returns_stored_uid() {
        assert_eq!(UidSlot::First(3).uid(), 3);
        assert_eq!(
            UidSlot::Later {
                first: 0,
                ordinal: 1,
                uid: 4
            }
            .uid(),
            4
        );
        assert_eq!(UidSlot::Duplicate { first: 0, uid: 5 }.uid(), 5);
    }

    #[test]
    fn test_assign_file_unscoped_matches_todays_overload_numbering() {
        let decls = [
            decl(NodeKind::Method, Some("A"), "save"),
            decl(NodeKind::Method, Some("A"), "save"),
            decl(NodeKind::Method, Some("A"), "save"),
            decl(NodeKind::Function, None, "f"),
            decl(NodeKind::Function, None, "f"),
        ];
        let ids = assign_file("A.java", &decls).unwrap();
        let base = uid::compute(NodeKind::Method, "A.java", Some("A"), "save");
        assert_eq!(ids[0].slot, UidSlot::First(base));
        assert_eq!(
            ids[1].slot,
            UidSlot::Later {
                first: 0,
                ordinal: 1,
                uid: uid::overload(base, 1)
            }
        );
        assert_eq!(
            ids[2].slot,
            UidSlot::Later {
                first: 0,
                ordinal: 2,
                uid: uid::overload(base, 2)
            }
        );
        let f = uid::compute(NodeKind::Function, "A.java", None, "f");
        assert_eq!(ids[4].slot, UidSlot::Duplicate { first: 3, uid: f });
        assert_eq!(
            ids[0].uniqueness,
            Uniqueness::Shared {
                position: 0,
                count: 3
            }
        );
        assert_eq!(
            ids[2].uniqueness,
            Uniqueness::Shared {
                position: 2,
                count: 3
            }
        );
    }

    /// `class Conv` and `class Conv<T>` in one file, in either order, with a
    /// third declaration that repeats the first one's arity.
    fn arity_file(arities: [u32; 3]) -> Vec<DeclIdentity> {
        let mut decls = vec![decl(NodeKind::Class, None, "Conv"); 3];
        for (d, arity) in decls.iter_mut().zip(arities) {
            d.arity = Some(arity);
        }
        assign_file("N/Conv.cs", &decls).unwrap()
    }

    /// Stands for: a later type whose generic arity no earlier one has keeps
    /// a node of its own under overload uid 1, in either declaration order,
    /// while a repeat of an arity is still the tombstoned duplicate of the
    /// one with that arity (codex ruling P-a, restricted shape (i)).
    #[test]
    fn test_assign_file_type_of_new_arity_later_same_arity_duplicate() {
        let base = uid::compute(NodeKind::Class, "N/Conv.cs", None, "Conv");
        for arities in [[0, 1, 0], [1, 0, 1]] {
            let ids = arity_file(arities);
            assert_eq!(ids[0].slot, UidSlot::First(base), "{arities:?}");
            assert_eq!(
                ids[1].slot,
                UidSlot::Later {
                    first: 0,
                    ordinal: 1,
                    uid: uid::overload(base, 1)
                },
                "{arities:?}: the other arity is a type of its own"
            );
            assert_eq!(
                ids[2].slot,
                UidSlot::Duplicate {
                    first: 0,
                    uid: base
                },
                "{arities:?}: a repeated arity tombstones against its twin"
            );
            assert!(
                ids.iter().all(|id| id.anchor == base),
                "{arities:?}: the arity enters no anchor"
            );
        }
    }

    /// Stands for: the twin of a repeated arity is the declaration of that
    /// arity, not the base uid's first declaration.
    #[test]
    fn test_assign_file_repeat_of_later_arity_duplicates_later_twin() {
        let ids = arity_file([0, 1, 1]);
        let base = uid::compute(NodeKind::Class, "N/Conv.cs", None, "Conv");
        assert_eq!(
            ids[2].slot,
            UidSlot::Duplicate {
                first: 1,
                uid: base
            }
        );
    }

    /// Stands for: a declaration with no arity (every language but C#)
    /// keeps today's rule, a same-uid type is a duplicate.
    #[test]
    fn test_assign_file_type_without_arity_duplicate() {
        let decls = [
            decl(NodeKind::Class, None, "Conv"),
            decl(NodeKind::Class, None, "Conv"),
        ];
        let ids = assign_file("Conv.java", &decls).unwrap();
        let base = uid::compute(NodeKind::Class, "Conv.java", None, "Conv");
        assert_eq!(
            ids[1].slot,
            UidSlot::Duplicate {
                first: 0,
                uid: base
            }
        );
    }

    /// `class Foo1` (arity `first`) holding property `foo`, then
    /// `class Foo1` (arity `second`) holding property `foo`, each property's
    /// arity scope its own class.
    fn twin_members(first: u32, second: u32) -> Vec<DeclIdentity> {
        let mut decls = vec![
            decl(NodeKind::Class, None, "Foo1"),
            decl(NodeKind::Property, Some("Foo1"), "foo"),
            decl(NodeKind::Class, None, "Foo1"),
            decl(NodeKind::Property, Some("Foo1"), "foo"),
        ];
        for (d, arity) in decls.iter_mut().zip([first, 0, second, 0]) {
            d.arity = Some(arity);
        }
        decls[1].arity_scope = Some(0);
        decls[3].arity_scope = Some(2);
        assign_file("N/Foo1.cs", &decls).unwrap()
    }

    /// Stands for: a member of a type of another generic arity is a
    /// declaration of its own (`Foo1<T>.foo` after `Foo1.foo`, codex ruling
    /// P-a2): overload uid 1 of the member's base uid, in either order. A
    /// member of a same-arity duplicate type stays the duplicate it was.
    #[test]
    fn test_assign_file_member_of_arity_twin_later_of_same_arity_duplicate() {
        let foo = uid::compute(NodeKind::Property, "N/Foo1.cs", Some("Foo1"), "foo");
        for (first, second) in [(0, 1), (1, 0)] {
            let ids = twin_members(first, second);
            assert_eq!(
                ids[3].slot,
                UidSlot::Later {
                    first: 1,
                    ordinal: 1,
                    uid: uid::overload(foo, 1)
                },
                "arity {first} then {second}"
            );
            assert_eq!(ids[3].anchor, foo, "the arity enters no anchor");
        }
        assert_eq!(
            twin_members(1, 1)[3].slot,
            UidSlot::Duplicate { first: 1, uid: foo }
        );
    }

    /// Stands for: the arity path runs outward through every enclosing
    /// type. `Outer.Inner.x` and `Outer<T>.Inner.x` differ only in the
    /// outer arity, yet each `Inner` and each `x` is a declaration of its
    /// own; with equal outer arities both repeat.
    #[test]
    fn test_assign_file_nested_member_outer_arity_differs_later() {
        let file = |outer: [u32; 2]| {
            let mut decls = vec![
                decl(NodeKind::Class, None, "Outer"),
                decl(NodeKind::Class, Some("Outer"), "Inner"),
                decl(NodeKind::Property, Some("Inner"), "x"),
                decl(NodeKind::Class, None, "Outer"),
                decl(NodeKind::Class, Some("Outer"), "Inner"),
                decl(NodeKind::Property, Some("Inner"), "x"),
            ];
            for (d, arity) in decls.iter_mut().zip([outer[0], 0, 0, outer[1], 0, 0]) {
                d.arity = Some(arity);
            }
            for (child, scope) in [(1, 0), (2, 1), (4, 3), (5, 4)] {
                decls[child].arity_scope = Some(scope);
            }
            assign_file("N/Outer.cs", &decls).unwrap()
        };
        let split = file([0, 1]);
        assert!(
            matches!(split[4].slot, UidSlot::Later { first: 1, .. }),
            "{:?}",
            split[4]
        );
        assert!(
            matches!(split[5].slot, UidSlot::Later { first: 2, .. }),
            "{:?}",
            split[5]
        );
        let same = file([1, 1]);
        assert!(matches!(same[3].slot, UidSlot::Duplicate { first: 0, .. }));
        assert!(matches!(same[4].slot, UidSlot::Duplicate { first: 1, .. }));
        assert!(matches!(same[5].slot, UidSlot::Duplicate { first: 2, .. }));
    }

    /// Stands for: an arity scope outside the slice is rejected like a bad
    /// parent, and a scope cycle (no caller builds one) has no arity path:
    /// the walk ends and the repeat is a declaration of its own.
    #[test]
    fn test_assign_file_bad_or_cyclic_arity_scope_rejected_or_terminates() {
        let mut decls = vec![decl(NodeKind::Property, Some("A"), "p"); 2];
        decls[0].arity = Some(0);
        decls[1].arity = Some(0);
        decls[1].arity_scope = Some(7);
        assert_eq!(
            assign_file("A.cs", &decls),
            Err(IdentityError::BadParent { decl: 1 })
        );
        decls[0].arity_scope = Some(1);
        decls[1].arity_scope = Some(0);
        let ids = assign_file("A.cs", &decls).unwrap();
        assert!(
            matches!(ids[1].slot, UidSlot::Later { first: 0, .. }),
            "{:?}",
            ids[1]
        );
    }

    #[test]
    fn test_assign_file_swapped_impls_keep_child_uids() {
        let file = |first: &'static str, second: &'static str| {
            let mut decls = vec![
                decl(NodeKind::Method, Some("A"), "fmt"),
                decl(NodeKind::Function, None, "pad"),
                decl(NodeKind::Method, Some("A"), "fmt"),
                decl(NodeKind::Function, None, "pad"),
            ];
            decls[0].discriminator = Some(Discriminator::ImplTrait(first));
            decls[2].discriminator = Some(Discriminator::ImplTrait(second));
            decls[1].parent = Some(0);
            decls[3].parent = Some(2);
            assign_file("src/a.rs", &decls).unwrap()
        };
        let before = file("Display", "Debug");
        let after = file("Debug", "Display");
        assert_eq!(before[1].slot.uid(), after[3].slot.uid());
        assert_eq!(before[3].slot.uid(), after[1].slot.uid());
        assert_eq!(before[0].anchor, after[2].anchor);
        assert!(before.iter().all(|d| d.uniqueness == Uniqueness::Unique));
    }

    #[test]
    fn test_assign_file_sibling_blocks_shared_not_duplicate() {
        let mut decls = vec![
            decl(NodeKind::Function, None, "outer"),
            decl(NodeKind::Function, None, "h"),
            decl(NodeKind::Function, None, "h"),
        ];
        decls[1].parent = Some(0);
        decls[2].parent = Some(0);
        let ids = assign_file("a.py", &decls).unwrap();
        assert!(matches!(
            ids[2].slot,
            UidSlot::Later {
                first: 1,
                ordinal: 1,
                ..
            }
        ));
        assert_ne!(ids[1].slot.uid(), ids[2].slot.uid());
        assert_eq!(
            ids[1].uniqueness,
            Uniqueness::Shared {
                position: 0,
                count: 2
            }
        );
    }

    #[test]
    fn test_assign_file_parent_after_child_resolves() {
        let mut decls = vec![
            decl(NodeKind::Function, None, "h"),
            decl(NodeKind::Function, None, "outer"),
        ];
        decls[0].parent = Some(1);
        let ids = assign_file("a.js", &decls).unwrap();
        assert_eq!(
            ids[0].slot.uid(),
            scoped_uid(ids[1].anchor, NodeKind::Function, "h")
        );
    }

    #[test]
    fn test_assign_file_bad_parents_rejected() {
        let mut cycle = vec![
            decl(NodeKind::Function, None, "a"),
            decl(NodeKind::Function, None, "b"),
        ];
        cycle[0].parent = Some(1);
        cycle[1].parent = Some(0);
        assert!(matches!(
            assign_file("a.rs", &cycle),
            Err(IdentityError::BadParent { .. })
        ));
        let mut out_of_range = vec![decl(NodeKind::Function, None, "a")];
        out_of_range[0].parent = Some(5);
        assert_eq!(
            assign_file("a.rs", &out_of_range),
            Err(IdentityError::BadParent { decl: 0 })
        );
    }

    #[test]
    fn test_assign_file_scoped_with_owner_or_overload_kind_rejected() {
        let mut owned = vec![
            decl(NodeKind::Function, None, "outer"),
            decl(NodeKind::Function, Some("outer"), "h"),
        ];
        owned[1].parent = Some(0);
        assert_eq!(
            assign_file("a.rs", &owned),
            Err(IdentityError::ScopedWithOwner { decl: 1 })
        );
        let mut method = vec![
            decl(NodeKind::Function, None, "outer"),
            decl(NodeKind::Method, None, "m"),
        ];
        method[1].parent = Some(0);
        assert_eq!(
            assign_file("a.rs", &method),
            Err(IdentityError::ScopedOverloadKind {
                kind: NodeKind::Method,
                decl: Some(1)
            })
        );
    }

    #[test]
    fn test_assign_file_discriminator_changes_scoped_child_uid() {
        let mut plain = vec![
            decl(NodeKind::Method, Some("A"), "fmt"),
            decl(NodeKind::Function, None, "pad"),
        ];
        plain[1].parent = Some(0);
        let mut tagged = plain.clone();
        tagged[0].discriminator = Some(Discriminator::ImplTrait("Display"));
        let (p, t) = (
            assign_file("a.rs", &plain).unwrap(),
            assign_file("a.rs", &tagged).unwrap(),
        );
        assert_eq!(p[0].slot, t[0].slot);
        assert_ne!(p[1].slot.uid(), t[1].slot.uid());
    }

    #[test]
    fn test_assign_file_empty_returns_empty() {
        assert_eq!(assign_file("a.rs", &[]), Ok(Vec::new()));
    }

    #[test]
    fn test_assign_file_backslash_path_rejected_with_or_without_decls() {
        let err = Err(IdentityError::NonCanonicalPath("src\\a.rs".into()));
        assert_eq!(assign_file("src\\a.rs", &[]), err);
        assert_eq!(
            assign_file("src\\a.rs", &[decl(NodeKind::Function, None, "f")]),
            err
        );
    }

    #[test]
    fn test_assign_file_anchor_equals_decl_key_anchor() {
        let mut decls = vec![
            decl(NodeKind::Method, Some("A"), "fmt"),
            decl(NodeKind::Method, Some("A"), "fmt"),
            decl(NodeKind::Function, None, "pad"),
        ];
        decls[1].discriminator = Some(Discriminator::ImplTrait("Display"));
        decls[2].parent = Some(1);
        let ids = assign_file("./a.rs", &decls).unwrap();
        let plain = DeclKey::unscoped(NodeKind::Method, "./a.rs", Some("A"), "fmt").unwrap();
        let tagged = plain
            .with_discriminator(Discriminator::ImplTrait("Display"))
            .unwrap();
        assert_eq!(ids[0].slot, UidSlot::First(plain.uid()));
        assert_eq!(
            ids[0].anchor,
            plain.uid(),
            "no discriminator: the anchor is the uid"
        );
        assert_eq!(ids[1].anchor, tagged.anchor());
        assert_ne!(ids[1].anchor, plain.uid());
        let child = DeclKey::scoped(NodeKind::Function, "./a.rs", "pad", tagged.anchor()).unwrap();
        assert_eq!(ids[2].slot, UidSlot::First(child.uid()));
        assert_eq!(ids[2].anchor, child.anchor());
    }

    #[test]
    fn test_pair_by_anchor_unique_pairs_shared_unpaired() {
        let base = [10, 20, 20, 30];
        let dirty = [30, 20, 20, 10, 40];
        assert_eq!(
            pair_by_anchor(&base, &dirty),
            vec![Some(3), None, None, Some(0), None]
        );
    }

    #[test]
    fn test_pair_by_anchor_dirty_repeat_unpaired() {
        assert_eq!(pair_by_anchor(&[10], &[10, 10]), vec![None, None]);
    }

    // ── lexical parent ──

    #[test]
    fn test_lexical_parents_nested_function_links_to_callable() {
        let decls = [
            (NodeKind::Function, (0, 0, 10, 0)),
            (NodeKind::Function, (1, 0, 3, 0)),
            (NodeKind::Variable, (4, 0, 4, 5)),
        ];
        assert_eq!(
            lexical_parents(&decls, &[NodeKind::Function]),
            vec![None, Some(0), None]
        );
    }

    #[test]
    fn test_lexical_parents_class_between_breaks_link() {
        let decls = [
            (NodeKind::Function, (0, 0, 20, 0)),
            (NodeKind::Class, (1, 0, 10, 0)),
            (NodeKind::Function, (2, 0, 3, 0)),
        ];
        assert_eq!(
            lexical_parents(&decls, &[NodeKind::Function]),
            vec![None, None, None]
        );
    }

    #[test]
    fn test_lexical_parents_skips_non_container_ancestor() {
        let decls = [
            (NodeKind::Method, (0, 0, 20, 0)),
            (NodeKind::Variable, (1, 0, 10, 0)),
            (NodeKind::Function, (2, 0, 3, 0)),
        ];
        assert_eq!(
            lexical_parents(&decls, &[NodeKind::Function]),
            vec![None, None, Some(0)]
        );
    }

    #[test]
    fn test_lexical_parents_no_opt_in_or_overload_kind_unscoped() {
        let decls = [
            (NodeKind::Function, (0, 0, 10, 0)),
            (NodeKind::Function, (1, 0, 3, 0)),
        ];
        assert_eq!(lexical_parents(&decls, &[]), vec![None, None]);
        let methods = [
            (NodeKind::Function, (0, 0, 10, 0)),
            (NodeKind::Method, (1, 0, 3, 0)),
        ];
        assert_eq!(
            lexical_parents(&methods, &[NodeKind::Method]),
            vec![None, None]
        );
    }

    #[test]
    fn test_lexical_parents_identical_spans_not_nested() {
        let decls = [
            (NodeKind::Function, (0, 0, 5, 0)),
            (NodeKind::Function, (0, 0, 5, 0)),
        ];
        assert_eq!(
            lexical_parents(&decls, &[NodeKind::Function]),
            vec![None, None]
        );
    }

    // ── target request ──

    #[test]
    fn test_parse_bare_name_qualifier_any() {
        let r = TargetRequest::parse("validate", None, None).unwrap();
        assert_eq!(r.qualifier(), &Qualifier::Any);
        assert_eq!(r.name(), "validate");
        assert_eq!(r.line(), None);
    }

    #[test]
    fn test_parse_dot_and_double_colon_equivalent() {
        let dot = TargetRequest::parse("a.b.f", None, None).unwrap();
        let colons = TargetRequest::parse("a::b::f", None, None).unwrap();
        let mixed = TargetRequest::parse("a::b.f", None, None).unwrap();
        assert_eq!(dot, colons);
        assert_eq!(dot, mixed);
        assert_eq!(
            dot.qualifier(),
            &Qualifier::Path(vec![Segment::Name("a".into()), Segment::Name("b".into())])
        );
    }

    #[test]
    fn test_parse_root_forms() {
        let colons = TargetRequest::parse("::helper", None, None).unwrap();
        let dot = TargetRequest::parse(".helper", None, None).unwrap();
        assert_eq!(colons.qualifier(), &Qualifier::Root);
        assert_eq!(colons, dot);
        assert_eq!(
            TargetRequest::parse("::a.helper", None, None),
            Err(TargetSyntaxError::RootWithQualifier)
        );
    }

    #[test]
    fn test_parse_impl_trait_segment() {
        let r = TargetRequest::parse("<A as fmt::Display>.fmt.pad", None, None).unwrap();
        assert_eq!(
            r.qualifier(),
            &Qualifier::Path(vec![
                Segment::ImplTrait {
                    self_ty: "A".into(),
                    trait_name: "fmt::Display".into()
                },
                Segment::Name("fmt".into()),
            ])
        );
        assert_eq!(r.name(), "pad");
    }

    #[test]
    fn test_parse_line_suffix() {
        let r = TargetRequest::parse("A.fmt@42", None, None).unwrap();
        assert_eq!(r.line(), Some(42));
        assert_eq!(r.name(), "fmt");
        assert_eq!(
            TargetRequest::parse("f@0", None, None),
            Err(TargetSyntaxError::BadLine("0".into()))
        );
        let entry = TargetRequest::parse("route@fx", None, None).unwrap();
        assert_eq!(entry.name(), "route@fx");
        assert_eq!(entry.line(), None);
    }

    #[test]
    fn test_parse_errors() {
        assert_eq!(
            TargetRequest::parse("", None, None),
            Err(TargetSyntaxError::Empty)
        );
        assert_eq!(
            TargetRequest::parse("a..b", None, None),
            Err(TargetSyntaxError::EmptySegment)
        );
        assert_eq!(
            TargetRequest::parse("a.", None, None),
            Err(TargetSyntaxError::EmptySegment)
        );
        assert_eq!(
            TargetRequest::parse("<A as B", None, None),
            Err(TargetSyntaxError::UnclosedAngle)
        );
        assert_eq!(
            TargetRequest::parse("<A as B>", None, None),
            Err(TargetSyntaxError::ImplTraitAsName)
        );
        assert_eq!(
            TargetRequest::parse("<A>.f", None, None),
            Err(TargetSyntaxError::BadImplTrait("<A>".into()))
        );
        assert_eq!(
            TargetRequest::parse("f", Some("gadget"), None),
            Err(TargetSyntaxError::UnknownKind("gadget".into()))
        );
    }

    #[test]
    fn test_parse_qualifier_generic_match_form_name_kept() {
        let r = TargetRequest::parse("Walk<'s>.next", None, None).unwrap();
        assert_eq!(
            r.qualifier(),
            &Qualifier::Path(vec![Segment::Name("Walk".into())])
        );
        let op = TargetRequest::parse("Foo.operator<", None, None).unwrap();
        assert_eq!(op.name(), "operator<");
    }

    #[test]
    fn test_parse_kind_and_file_filters() {
        let r = TargetRequest::parse("load", Some("method"), Some(".\\src\\a.rs")).unwrap();
        assert_eq!(r.kind(), Some(NodeKind::Method));
        assert_eq!(r.file(), Some("src/a.rs"));
        let empty = TargetRequest::parse("load", None, Some("")).unwrap();
        assert_eq!(empty.file(), None);
    }

    #[test]
    fn test_render_round_trips_through_parse() {
        for text in ["f", "::f", "a.b.f", "<A as Display>.fmt.pad@7", "A.fmt@3"] {
            let r = TargetRequest::parse(text, None, None).unwrap();
            assert_eq!(
                TargetRequest::parse(&r.render(), None, None).unwrap(),
                r,
                "{text}"
            );
        }
        assert_eq!(
            TargetRequest::parse("a::b::f", None, None)
                .unwrap()
                .render(),
            "a.b.f"
        );
    }

    #[test]
    fn test_qualifier_matches_segment_bounded_suffix() {
        let path = PathSegment::from_owner("pkg.Foo");
        assert!(Qualifier::Path(vec![Segment::Name("Foo".into())]).matches(&path));
        assert!(Qualifier::Path(vec![
            Segment::Name("pkg".into()),
            Segment::Name("Foo".into())
        ])
        .matches(&path));
        assert!(!Qualifier::Path(vec![Segment::Name("oo".into())]).matches(&path));
        assert!(!Qualifier::Root.matches(&path));
        assert!(Qualifier::Root.matches(&[]));
        assert!(Qualifier::Any.matches(&path));
    }

    #[test]
    fn test_qualifier_impl_trait_matches_only_that_trait() {
        let display = [
            PathSegment {
                name: "A",
                impl_trait: Some("fmt::Display"),
            },
            seg("fmt"),
        ];
        let debug = [
            PathSegment {
                name: "A",
                impl_trait: Some("Debug"),
            },
            seg("fmt"),
        ];
        let want = TargetRequest::parse("<A as Display>.fmt.pad", None, None).unwrap();
        assert!(want.qualifier().matches(&display));
        assert!(!want.qualifier().matches(&debug));
        let plain = TargetRequest::parse("A.fmt.pad", None, None).unwrap();
        assert!(plain.qualifier().matches(&display) && plain.qualifier().matches(&debug));
    }

    #[test]
    fn test_path_segment_from_owner_splits_both_separators() {
        let segs = PathSegment::from_owner("a::b.C");
        let names: Vec<&str> = segs.iter().map(|s| s.name).collect();
        assert_eq!(names, ["a", "b", "C"]);
        assert!(PathSegment::from_owner("").is_empty());
    }

    #[test]
    fn test_path_segment_from_owner_depth_aware() {
        let segs = PathSegment::from_owner("a::Conv<b::C>.D");
        let names: Vec<&str> = segs.iter().map(|s| s.name).collect();
        assert_eq!(names, ["a", "Conv<b::C>", "D"]);
    }

    #[test]
    fn test_qualifier_impl_trait_generic_trait_text_exact() {
        let conv = |t: &'static str| {
            [PathSegment {
                name: "A",
                impl_trait: Some(t),
            }]
        };
        let (u8_path, u16_path) = (conv("Conv<u8>"), conv("Conv<u16>"));
        let exact = TargetRequest::parse("<A as Conv<u8>>.f", None, None).unwrap();
        assert!(exact.qualifier().matches(&u8_path));
        assert!(!exact.qualifier().matches(&u16_path));
        let spaced = TargetRequest::parse("<A as Conv< u8 >>.f", None, None).unwrap();
        assert!(spaced.qualifier().matches(&u8_path));
        let any = TargetRequest::parse("<A as Conv>.f", None, None).unwrap();
        assert!(any.qualifier().matches(&u8_path) && any.qualifier().matches(&u16_path));
    }

    #[test]
    fn test_parse_literal_name_anonymous_closure() {
        let name = TargetRequest::parse("`<anonymous:3:4>`", None, None).unwrap();
        assert_eq!(name.qualifier(), &Qualifier::Any);
        assert_eq!(name.name(), "<anonymous:3:4>");
        let child = TargetRequest::parse("::`<anonymous:3:4>`@3", None, None).unwrap();
        assert_eq!(
            (child.qualifier(), child.name(), child.line()),
            (&Qualifier::Root, "<anonymous:3:4>", Some(3))
        );
        let nested = TargetRequest::parse("run.`<anonymous:3:4>`.helper", None, None).unwrap();
        assert_eq!(
            nested.qualifier(),
            &Qualifier::Path(vec![
                Segment::Name("run".into()),
                Segment::Literal("<anonymous:3:4>".into())
            ])
        );
        let tick = TargetRequest::parse("`a``b@1`", None, None).unwrap();
        assert_eq!((tick.name(), tick.line()), ("a`b@1", None));
        assert_eq!(
            TargetRequest::parse("`<anonymous:3:4>", None, None),
            Err(TargetSyntaxError::UnclosedQuote)
        );
        assert_eq!(
            TargetRequest::parse("``", None, None),
            Err(TargetSyntaxError::EmptySegment)
        );
        assert!(matches!(
            TargetRequest::parse("<anonymous:3:4>.helper", None, None),
            Err(TargetSyntaxError::BadImplTrait(_))
        ));
    }

    #[test]
    fn test_qualifier_literal_matches_exact_text_only() {
        let walk = [seg("Walk<'s, 't>")];
        let plain = [seg("Walk")];
        let literal = TargetRequest::parse("`Walk<'s, 't>`.next", None, None).unwrap();
        assert!(literal.qualifier().matches(&walk));
        assert!(!literal.qualifier().matches(&plain));
        let name = TargetRequest::parse("Walk.next", None, None).unwrap();
        assert!(name.qualifier().matches(&walk) && name.qualifier().matches(&plain));
    }

    #[test]
    fn test_file_match_tiers() {
        assert_eq!(
            file_match("cmd/main.go", "cmd/main.go"),
            Some(FileMatch::Equal)
        );
        assert_eq!(
            file_match("cmd/main.go", "tools/cmd/main.go"),
            Some(FileMatch::Suffix)
        );
        assert_eq!(
            file_match("main", "cmd/main.go"),
            Some(FileMatch::Substring)
        );
        assert_eq!(
            file_match("cmd/", "cmd/main.go"),
            Some(FileMatch::Substring)
        );
        assert_eq!(file_match("x.go", "cmd/main.go"), None);
    }

    #[test]
    fn test_normalize_file_needle_slashes_and_dot_prefix() {
        assert_eq!(normalize_file_needle(".\\src\\a.rs"), "src/a.rs");
        assert_eq!(normalize_file_needle("src/a.rs"), "src/a.rs");
    }

    #[test]
    fn test_select_file_keeps_best_tier() {
        let named = [
            facts(
                0,
                NodeKind::Function,
                "tools/cmd/main.go",
                1,
                vec![],
                "main",
            ),
            facts(1, NodeKind::Function, "cmd/main.go", 1, vec![], "main"),
        ];
        let r = TargetRequest::parse("main", None, Some("cmd/main.go")).unwrap();
        assert_eq!(select(&r, &named), vec![1]);
    }

    #[test]
    fn test_resolve_target_unique() {
        let named = [
            facts(
                4,
                NodeKind::Method,
                "a.rs",
                3,
                PathSegment::from_owner("A"),
                "load",
            ),
            facts(
                9,
                NodeKind::Method,
                "b.rs",
                9,
                PathSegment::from_owner("B"),
                "load",
            ),
        ];
        let r = TargetRequest::parse("A.load", None, None).unwrap();
        match resolve_target(&r, &named) {
            TargetResolution::Unique(c) => {
                assert_eq!(c.node, 4);
                assert_eq!(c.display, "A.load");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn test_resolve_target_not_found_counts_named() {
        let named = [facts(
            4,
            NodeKind::Method,
            "a.rs",
            3,
            PathSegment::from_owner("A"),
            "load",
        )];
        let by_kind = TargetRequest::parse("load", Some("function"), None).unwrap();
        assert_eq!(
            resolve_target(&by_kind, &named),
            TargetResolution::NotFound { named: 1 }
        );
        let other = TargetRequest::parse("save", None, None).unwrap();
        assert_eq!(
            resolve_target(&other, &named),
            TargetResolution::NotFound { named: 0 }
        );
        assert_eq!(
            resolve_target(&other, &[]),
            TargetResolution::NotFound { named: 0 }
        );
    }

    #[test]
    fn test_resolve_target_ambiguous_reusable_targets_select_alone() {
        let display = PathSegment {
            name: "A",
            impl_trait: Some("Display"),
        };
        let debug = PathSegment {
            name: "A",
            impl_trait: Some("Debug"),
        };
        let named = [
            facts(
                1,
                NodeKind::Function,
                "a.rs",
                5,
                vec![display, seg("fmt")],
                "pad",
            ),
            facts(
                2,
                NodeKind::Function,
                "a.rs",
                9,
                vec![debug, seg("fmt")],
                "pad",
            ),
            facts(3, NodeKind::Function, "a.rs", 20, vec![], "pad"),
            facts(4, NodeKind::Function, "b.rs", 1, vec![], "pad"),
            facts(5, NodeKind::Function, "b.rs", 1, vec![], "pad"),
        ];
        let r = TargetRequest::parse("pad", None, None).unwrap();
        let TargetResolution::Ambiguous(amb) = resolve_target(&r, &named) else {
            panic!("expected an ambiguity");
        };
        assert_eq!(amb.total(), 5);
        for (i, c) in amb.candidates().iter().enumerate() {
            match &c.reusable {
                Some(req) => assert_eq!(select(req, &named), vec![i], "{c:?}"),
                None => assert!(i >= 3, "only the two identical b.rs nodes lack one: {c:?}"),
            }
        }
        let first = amb.candidates()[0].reusable.as_ref().unwrap();
        assert_eq!(first.render(), "<A as Display>.fmt.pad");
        let root = amb.candidates()[2].reusable.as_ref().unwrap();
        assert_eq!(
            (root.render().as_str(), root.file()),
            ("::pad", Some("a.rs"))
        );
        assert_eq!(amb.candidates()[3].reusable, None);
        assert_eq!(amb.candidates()[4].reusable, None);
    }

    #[test]
    fn test_resolve_target_top_level_and_nested_helper_bare_name_ambiguous() {
        let named = [
            facts(1, NodeKind::Function, "a.rs", 1, vec![], "helper"),
            facts(
                2,
                NodeKind::Function,
                "a.rs",
                5,
                vec![seg("outer")],
                "helper",
            ),
        ];
        let bare = TargetRequest::parse("helper", None, None).unwrap();
        let TargetResolution::Ambiguous(amb) = resolve_target(&bare, &named) else {
            panic!("a bare name must not pick the top-level helper");
        };
        assert_eq!(amb.total(), 2);
        assert!(matches!(
            amb.to_error(&bare),
            EcpError::AmbiguousSymbol {
                owner_form: true,
                ..
            }
        ));
        let root = TargetRequest::parse("::helper", None, None).unwrap();
        assert!(matches!(
            resolve_target(&root, &named),
            TargetResolution::Unique(Candidate { node: 1, .. })
        ));
        let nested = TargetRequest::parse("outer.helper", None, None).unwrap();
        assert!(matches!(
            resolve_target(&nested, &named),
            TargetResolution::Unique(Candidate { node: 2, .. })
        ));
    }

    #[test]
    fn test_resolve_target_overloads_on_one_line_no_reusable() {
        let named = [
            facts(1, NodeKind::Method, "a.cpp", 3, vec![seg("A")], "f"),
            facts(2, NodeKind::Method, "a.cpp", 3, vec![seg("A")], "f"),
        ];
        let r = TargetRequest::parse("A.f", None, None).unwrap();
        let TargetResolution::Ambiguous(amb) = resolve_target(&r, &named) else {
            panic!("expected an ambiguity");
        };
        assert!(amb.candidates().iter().all(|c| c.reusable.is_none()));
    }

    fn bare(name: &str) -> TargetRequest {
        TargetRequest::parse(name, None, None).unwrap()
    }

    fn ctor(node: u32, file: &str, line: u32) -> CandidateFacts<'_> {
        facts(
            node,
            NodeKind::Constructor,
            file,
            line,
            vec![seg("Foo")],
            "Foo",
        )
    }

    fn class<'a>(
        node: u32,
        file: &'a str,
        lines: (u32, u32),
        qualifier: Vec<PathSegment<'a>>,
    ) -> CandidateFacts<'a> {
        CandidateFacts {
            end_line: lines.1,
            ..facts(node, NodeKind::Class, file, lines.0, qualifier, "Foo")
        }
    }

    #[test]
    fn test_select_bare_type_single_constructor_selects_type() {
        let named = [class(0, "a.cs", (1, 4), vec![]), ctor(1, "a.cs", 2)];
        assert_eq!(select(&bare("Foo"), &named), vec![0]);
    }

    #[test]
    fn test_select_bare_type_two_overloads_both_drop() {
        let named = [
            class(0, "a.cs", (1, 4), vec![seg("N")]),
            ctor(1, "a.cs", 2),
            ctor(2, "a.cs", 3),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0]);
    }

    #[test]
    fn test_select_bare_nested_type_selects_nested_type() {
        let named = [
            class(0, "a.java", (2, 5), vec![seg("Outer")]),
            ctor(1, "a.java", 3),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0]);
    }

    #[test]
    fn test_select_bare_two_types_one_file_drops_enclosed_constructor() {
        let named = [
            class(0, "a.java", (1, 4), vec![]),
            class(1, "a.java", (5, 8), vec![seg("Outer")]),
            ctor(2, "a.java", 6),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 1]);
    }

    /// Stands for: a constructor declared outside its class encloses in no
    /// same-file type, so it keeps competing. Failed before: C++
    /// `namespace B { Foo::Foo() {} }` below `namespace A { class Foo {}; }`
    /// dropped and the bare name selected `A::Foo`.
    #[test]
    fn test_select_bare_out_of_class_constructor_stays() {
        let named = [
            class(0, "impl.cpp", (3, 3), vec![seg("A")]),
            ctor(1, "impl.cpp", 6),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 1]);
    }

    fn spanned<'a>(
        facts: CandidateFacts<'a>,
        end_line: u32,
        columns: (u32, u32),
    ) -> CandidateFacts<'a> {
        CandidateFacts {
            end_line,
            columns,
            ..facts
        }
    }

    /// Stands for: an out-of-class definition beside a same-named class on
    /// one line is not that class's constructor. Failed at 15f1bbe1, which
    /// compared lines only: `namespace A { class Foo {}; } namespace B {
    /// Foo::Foo() {} }` on one line dropped the constructor (codex
    /// counterexample).
    #[test]
    fn test_select_bare_out_of_class_constructor_on_type_line_stays() {
        let named = [
            spanned(class(0, "impl.cpp", (2, 2), vec![seg("A")]), 2, (14, 26)),
            spanned(ctor(1, "impl.cpp", 2), 2, (44, 57)),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 1]);
    }

    #[test]
    fn test_select_bare_constructor_after_type_end_on_last_line_stays() {
        let named = [
            spanned(class(0, "a.cpp", (1, 3), vec![]), 3, (0, 2)),
            spanned(ctor(1, "a.cpp", 3), 3, (4, 18)),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 1]);
    }

    /// Stands for: a Kotlin or C# primary constructor sits on its class's
    /// line, inside the class's span, and drops like any own constructor.
    #[test]
    fn test_select_bare_primary_constructor_on_type_line_selects_type() {
        let named = [
            spanned(class(0, "Foo.kt", (1, 4), vec![]), 4, (0, 1)),
            spanned(ctor(1, "Foo.kt", 1), 1, (9, 21)),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0]);
    }

    #[test]
    fn test_select_bare_type_below_constructor_keeps_constructor() {
        let named = [ctor(0, "impl.cpp", 2), class(1, "impl.cpp", (5, 8), vec![])];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 1]);
    }

    #[test]
    fn test_select_bare_same_named_types_two_files_stay_ambiguous() {
        let named = [
            class(0, "a.cs", (1, 4), vec![]),
            ctor(1, "a.cs", 2),
            class(2, "b.cs", (1, 4), vec![]),
            ctor(3, "b.cs", 2),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 2]);
    }

    #[test]
    fn test_select_bare_unrelated_function_namesake_stays_ambiguous() {
        let named = [
            class(0, "a.cs", (1, 4), vec![]),
            ctor(1, "a.cs", 2),
            facts(2, NodeKind::Function, "b.py", 1, vec![], "Foo"),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 2]);
    }

    #[test]
    fn test_select_bare_constructor_in_other_file_than_type_stays() {
        let named = [class(0, "foo.h", (1, 4), vec![]), ctor(1, "foo.cpp", 2)];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 1]);
    }

    #[test]
    fn test_select_bare_constructor_with_unrecorded_owner_stays() {
        let named = [
            class(0, "a.cs", (1, 4), vec![]),
            facts(1, NodeKind::Constructor, "a.cs", 2, vec![], "Foo"),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 1]);
    }

    #[test]
    fn test_select_constructor_owned_by_other_name_stays() {
        let named = [
            class(0, "a.cs", (1, 4), vec![]),
            facts(1, NodeKind::Constructor, "a.cs", 2, vec![seg("Bar")], "Foo"),
        ];
        assert_eq!(select(&bare("Foo"), &named), vec![0, 1]);
    }

    #[test]
    fn test_select_explicit_kind_constructor_keeps_overloads() {
        let named = [
            class(0, "a.cs", (1, 4), vec![]),
            ctor(1, "a.cs", 2),
            ctor(2, "a.cs", 3),
        ];
        let r = TargetRequest::parse("Foo", Some("constructor"), None).unwrap();
        assert_eq!(select(&r, &named), vec![1, 2]);
    }

    #[test]
    fn test_select_line_selector_reaches_constructor() {
        let named = [
            class(0, "a.cs", (1, 4), vec![]),
            ctor(1, "a.cs", 2),
            ctor(2, "a.cs", 3),
        ];
        let r = TargetRequest::parse("Foo@3", None, None).unwrap();
        assert_eq!(select(&r, &named), vec![2]);
    }

    #[test]
    fn test_select_qualified_foo_foo_keeps_overload_ambiguity() {
        let named = [
            class(0, "a.cs", (1, 4), vec![]),
            ctor(1, "a.cs", 2),
            ctor(2, "a.cs", 3),
        ];
        assert_eq!(select(&bare("Foo.Foo"), &named), vec![1, 2]);
    }

    #[test]
    fn test_resolve_target_bare_type_with_constructor_is_unique_and_ctor_reusable() {
        let named = [
            class(0, "a.cs", (1, 4), vec![]),
            ctor(1, "a.cs", 2),
            ctor(2, "a.cs", 3),
        ];
        match resolve_target(&bare("Foo"), &named) {
            TargetResolution::Unique(c) => assert_eq!((c.node, c.kind), (0, NodeKind::Class)),
            other => panic!("{other:?}"),
        }
        let TargetResolution::Ambiguous(amb) = resolve_target(&bare("Foo.Foo"), &named) else {
            panic!("expected an ambiguity");
        };
        for c in amb.candidates() {
            let req = c.reusable.as_ref().expect("each overload has a target");
            assert_eq!(select(req, &named), vec![c.node as usize]);
        }
    }

    #[test]
    fn test_reusable_render_parse_selects_only_original_every_shape() {
        let conv = |t: &'static str| PathSegment {
            name: "A",
            impl_trait: Some(t),
        };
        let all = [
            facts(0, NodeKind::Function, "a.rs", 3, vec![], "<anonymous:3:4>"),
            facts(1, NodeKind::Function, "b.rs", 3, vec![], "<anonymous:3:4>"),
            facts(
                2,
                NodeKind::Function,
                "a.rs",
                4,
                vec![seg("<anonymous:3:4>")],
                "helper",
            ),
            facts(3, NodeKind::Function, "a.rs", 10, vec![], "helper"),
            facts(
                4,
                NodeKind::Function,
                "a.rs",
                12,
                vec![seg("run")],
                "helper",
            ),
            facts(5, NodeKind::Method, "a.rs", 20, vec![conv("Conv<u8>")], "f"),
            facts(
                6,
                NodeKind::Method,
                "a.rs",
                25,
                vec![conv("Conv<u16>")],
                "f",
            ),
            facts(
                7,
                NodeKind::Method,
                "a.rs",
                30,
                vec![seg("Walk<'s, 't>")],
                "next",
            ),
            facts(8, NodeKind::Method, "a.rs", 35, vec![seg("Walk")], "next"),
            facts(
                9,
                NodeKind::Method,
                "c.cpp",
                5,
                vec![seg("Foo")],
                "operator<",
            ),
            facts(
                10,
                NodeKind::Method,
                "c.cpp",
                9,
                vec![seg("Foo")],
                "operator<",
            ),
            facts(11, NodeKind::Function, "a.rs", 40, vec![seg("a`b")], "g"),
        ];
        let picked: Vec<usize> = (0..all.len()).collect();
        let amb = Ambiguity::new(&all, &picked);
        for (i, c) in amb.candidates().iter().enumerate() {
            let req = c.reusable.as_ref().unwrap_or_else(|| panic!("{c:?}"));
            let text = req.render();
            let back =
                TargetRequest::parse(&text, req.kind().map(NodeKind::as_str), req.file()).unwrap();
            assert_eq!(&back, req, "{text}");
            assert_eq!(select(&back, &all), vec![i], "{text}");
        }
        let text = |i: usize| amb.candidates()[i].reusable.as_ref().unwrap().render();
        assert_eq!(text(0), "::`<anonymous:3:4>`");
        assert_eq!(text(2), "`<anonymous:3:4>`.helper");
        assert_eq!(text(3), "::helper");
        assert_eq!(text(5), "<A as Conv<u8>>.f");
        assert_eq!(text(7), "`Walk<'s, 't>`.next");
        assert_eq!(text(9), "Foo.`operator<`@5");
        assert_eq!(text(11), "`a``b`.g");
    }

    #[test]
    fn test_select_explicit_kind_impl_keeps_impl() {
        let named = [
            facts(1, NodeKind::Struct, "a.rs", 1, vec![], "A"),
            facts(2, NodeKind::Impl, "a.rs", 5, vec![], "A"),
        ];
        let r = TargetRequest::parse("A", Some("impl"), None).unwrap();
        assert_eq!(select(&r, &named), vec![1]);
    }

    #[test]
    fn test_ambiguity_to_error_matches_todays_fields() {
        let named = [
            facts(
                1,
                NodeKind::Method,
                "a.rs",
                3,
                PathSegment::from_owner("A"),
                "load",
            ),
            facts(
                2,
                NodeKind::Method,
                "b.rs",
                9,
                PathSegment::from_owner("B"),
                "load",
            ),
        ];
        let r = TargetRequest::parse("load", Some("method"), None).unwrap();
        let TargetResolution::Ambiguous(amb) = resolve_target(&r, &named) else {
            panic!("expected an ambiguity");
        };
        match amb.to_error(&r) {
            EcpError::AmbiguousSymbol {
                name,
                count,
                candidates,
                file_given,
                kind_given,
                owner_form,
            } => {
                assert_eq!(name, "load");
                assert_eq!(count, 2);
                assert_eq!(
                    candidates.as_deref(),
                    Some("  a.rs,Method,3\n  b.rs,Method,9")
                );
                assert!(!file_given && kind_given && owner_form);
            }
            other => panic!("{other:?}"),
        }
    }

    // ── route and handler ──

    #[test]
    fn test_route_key_uid_equals_todays_route_uid() {
        let key = RouteKey::new("src/app.ts", "get", "/users").unwrap();
        assert_eq!(key.verb(), "GET");
        assert_eq!(key.name(), "GET /users");
        assert_eq!(
            key.uid(),
            uid::compute(NodeKind::Route, "src/app.ts", None, "GET /users")
        );
    }

    #[test]
    fn test_route_key_invalid_inputs_rejected() {
        assert_eq!(
            RouteKey::new("src\\a.ts", "GET", "/x"),
            Err(RouteKeyError::NonCanonicalFile("src\\a.ts".into()))
        );
        assert_eq!(
            RouteKey::new("a.ts", " ", "/x"),
            Err(RouteKeyError::EmptyVerb)
        );
        assert_eq!(
            RouteKey::new("a.ts", "GE T", "/x"),
            Err(RouteKeyError::VerbWithWhitespace("GE T".into()))
        );
        assert_eq!(
            RouteKey::new("a.ts", "GET", "  "),
            Err(RouteKeyError::EmptyPath)
        );
    }

    #[test]
    fn test_route_key_split_name_inverts_name() {
        let key = RouteKey::new("a.proto", "GRPC", "/pkg.Svc/Get").unwrap();
        assert_eq!(
            RouteKey::split_name(&key.name()),
            Some(("GRPC", "/pkg.Svc/Get"))
        );
        assert_eq!(RouteKey::split_name("GET"), None);
    }

    #[test]
    fn test_join_route_path_slash_only_path_keeps_trailing_slash() {
        assert_eq!(join_route_path("/api", "/"), "/api/");
        assert_eq!(join_route_path("/api/", "/"), "/api/");
        assert_eq!(join_route_path("/api/", ""), "/api/");
        assert_eq!(join_route_path("api", " / "), "/api/");
        assert_ne!(join_route_path("/api", "/"), join_route_path("/api", ""));
    }

    #[test]
    fn test_join_route_path_one_slash_at_joint() {
        assert_eq!(join_route_path("/api/", "/users"), "/api/users");
        assert_eq!(join_route_path("api", "users"), "/api/users");
        assert_eq!(join_route_path("", "/users"), "/users");
        assert_eq!(join_route_path("/api", ""), "/api");
        assert_eq!(join_route_path("", ""), "/");
        assert_eq!(join_route_path("/", "/"), "/");
        assert_eq!(join_route_path("/api", "users/"), "/api/users/");
    }

    #[test]
    fn test_handler_ref_handler_kind_values() {
        assert_eq!(HandlerRef::Declaration { uid: 1 }.handler_kind(), "named");
        assert_eq!(
            HandlerRef::Closure { uid: 1 }.handler_kind(),
            "inline_anonymous"
        );
        assert_eq!(
            HandlerRef::Inline { line: 3 }.handler_kind(),
            "inline_anonymous"
        );
        assert_eq!(
            HandlerRef::Unresolved { name: "h".into() }.handler_kind(),
            "unresolved"
        );
        assert_eq!(HandlerRef::Absent.handler_kind(), "absent");
    }

    #[test]
    fn test_route_binding_route_key_excludes_handler() {
        let route = RouteKey::new("a.ts", "GET", "/x").unwrap();
        let a = RouteBinding {
            route: route.clone(),
            handler: HandlerRef::Declaration { uid: 1 },
        };
        let b = RouteBinding {
            route,
            handler: HandlerRef::Inline { line: 2 },
        };
        assert_eq!(a.route.uid(), b.route.uid());
    }
}
