//! Synthetic `@import<N>` imports of literal dynamic module loads, and the
//! names that hold a loaded module (T1, FU-2026-11-01-c07a40e90e36).
//!
//! TypeScript, JavaScript and Python share this record and its lookups. Each
//! language keeps its own candidate collector and its own table of read
//! positions, because those follow the grammar.
//!
//! Alias `@import<N>`: `N` indexes the specifier by first occurrence in the
//! source. It has no `.`, so a `receiver.member` callee text splits cleanly.

use crate::calls::{range_width, start_pos, Pos, PosRange};
use ecp_core::analyzer::types::RawImport;
use std::collections::HashMap;
use tree_sitter::Node;

/// A name bound to a dynamically loaded module, valid from the end of its
/// declaration to the end of its scope.
#[derive(Debug)]
pub(crate) struct DynamicBinding {
    pub(crate) name: String,
    pub(crate) range: PosRange,
    pub(crate) alias: String,
    /// The export a destructured name holds; `None` for the namespace name.
    pub(crate) member: Option<String>,
}

/// The synthetic imports of a file's literal dynamic loads, the names that
/// hold a loaded module, and the range of every load call that got an import.
#[derive(Debug, Default)]
pub struct DynamicImports {
    pub imports: Vec<RawImport>,
    pub(crate) bindings: Vec<DynamicBinding>,
    pub(crate) loads: Vec<PosRange>,
}

impl DynamicImports {
    fn lookup(&self, pos: Pos, name: &str) -> Option<&DynamicBinding> {
        self.bindings
            .iter()
            .filter(|b| b.name == name && b.range.0 <= pos && pos <= b.range.1)
            .min_by_key(|b| range_width(b.range))
    }

    /// `m.prop()` through a namespace name: `@import<N>.prop`.
    pub fn member_callee(&self, pos: Pos, receiver: &str, prop: &str) -> Option<String> {
        let binding = self.lookup(pos, receiver)?;
        binding
            .member
            .is_none()
            .then(|| format!("{}.{prop}", binding.alias))
    }

    /// `execute()` through a destructured name: `@import<N>.run`.
    pub fn callable_callee(&self, pos: Pos, name: &str) -> Option<String> {
        let binding = self.lookup(pos, name)?;
        binding
            .member
            .as_ref()
            .map(|member| format!("{}.{member}", binding.alias))
    }

    /// The callee text of `call`, whose function is the identifier
    /// `function`: the export a destructured name holds, else the name.
    pub fn bare_callee(&self, call: Node<'_>, function: Node<'_>, source: &[u8]) -> Option<String> {
        let name = function.utf8_text(source).ok()?;
        Some(
            self.callable_callee(start_pos(call), name)
                .unwrap_or_else(|| name.to_string()),
        )
    }

    /// Whether `pos` lies in a load call that got a synthetic import.
    pub fn loaded_at(&self, pos: Pos) -> bool {
        self.loads
            .iter()
            .any(|&(start, end)| start <= pos && pos <= end)
    }
}

/// One namespace `RawImport` per distinct specifier of `specifiers` (in
/// source order), and the alias each specifier got.
pub(crate) fn synthetic_imports<'s>(
    specifiers: impl IntoIterator<Item = &'s str>,
) -> (Vec<RawImport>, HashMap<&'s str, String>) {
    let mut aliases: HashMap<&str, String> = HashMap::new();
    let mut imports: Vec<RawImport> = Vec::new();
    for specifier in specifiers {
        aliases.entry(specifier).or_insert_with(|| {
            let alias = format!("@import{}", imports.len());
            imports.push(RawImport {
                source: specifier.to_string(),
                imported_name: "*".to_string(),
                alias: Some(alias.clone()),
                binding_kind: None,
            });
            alias
        });
    }
    (imports, aliases)
}

/// Whether `node` is the node `parent` holds in `field`.
pub(crate) fn is_field(parent: Node<'_>, field: &str, node: Node<'_>) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|child| child.id() == node.id())
}

/// Whether `source` spells one of `words`. A load spells its loader's name,
/// so a file that spells none holds no load; text that is not UTF-8 never
/// counts as spelling none.
pub(crate) fn may_load(source: &[u8], words: &[&str]) -> bool {
    !std::str::from_utf8(source).is_ok_and(|text| !words.iter().any(|w| text.contains(w)))
}

/// The first two leaves in `scope` that spell one of `names` and that
/// `binds` accepts (a mention that binds or writes the name), per name. One
/// walk serves every name; it stops once each name has two.
pub(crate) fn binding_mentions<'t, 'n>(
    scope: Node<'t>,
    names: &[&'n str],
    source: &[u8],
    binds: impl Fn(Node<'t>) -> bool,
) -> HashMap<&'n str, Vec<Node<'t>>> {
    let mut found: HashMap<&str, Vec<Node<'t>>> =
        names.iter().map(|&name| (name, Vec::new())).collect();
    let mut open = found.len();
    let mut stack = if open == 0 { Vec::new() } else { vec![scope] };
    while let Some(n) = stack.pop() {
        if n.child_count() == 0 {
            if let Some(mentions) = n.utf8_text(source).ok().and_then(|t| found.get_mut(t)) {
                if mentions.len() < 2 && binds(n) {
                    mentions.push(n);
                    if mentions.len() == 2 {
                        open -= 1;
                        if open == 0 {
                            break;
                        }
                    }
                }
            }
            continue;
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    found
}

/// Whether a name is bound or written outside the byte range `own`, given
/// its [`binding_mentions`]. The declaration's own name is at most one
/// mention, so a second one always lies outside it.
pub(crate) fn rebound(mentions: Option<&Vec<Node<'_>>>, own: (usize, usize)) -> bool {
    mentions.is_some_and(|found| {
        found.len() > 1
            || found
                .iter()
                .any(|n| !(own.0 <= n.start_byte() && n.end_byte() <= own.1))
    })
}
