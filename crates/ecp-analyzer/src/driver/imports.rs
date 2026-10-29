//! Import records and the two sort-and-dedupe orders in use.

use ecp_core::analyzer::types::{BindingKind, RawImport};

/// `binding_kind` has no default: C and TS / JS pass a kind, the rest `None`.
pub fn raw(
    source: String,
    imported_name: String,
    alias: Option<String>,
    binding_kind: Option<BindingKind>,
) -> RawImport {
    RawImport {
        source,
        imported_name,
        alias,
        binding_kind,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportOrder {
    /// Go: sort by `(source, imported_name)` and dedupe on both. The sort is
    /// stable and `alias` is no key, so the first alias of a pair survives.
    SourceName,
    /// C++: sort by `(imported_name, source, alias)` and dedupe on all three.
    NameSourceAlias,
}

impl ImportOrder {
    pub fn sort_dedup(self, imports: &mut Vec<RawImport>) {
        match self {
            ImportOrder::SourceName => {
                imports.sort_by(|a, b| {
                    a.source
                        .cmp(&b.source)
                        .then(a.imported_name.cmp(&b.imported_name))
                });
                imports.dedup_by(|a, b| a.source == b.source && a.imported_name == b.imported_name);
            }
            ImportOrder::NameSourceAlias => {
                imports.sort_by(|a, b| {
                    a.imported_name
                        .cmp(&b.imported_name)
                        .then(a.source.cmp(&b.source))
                        .then(a.alias.cmp(&b.alias))
                });
                imports.dedup_by(|a, b| {
                    a.imported_name == b.imported_name && a.source == b.source && a.alias == b.alias
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(source: &str, name: &str, alias: Option<&str>) -> RawImport {
        raw(source.into(), name.into(), alias.map(str::to_string), None)
    }

    #[test]
    fn test_import_raw_keeps_binding_kind() {
        for kind in [Some(BindingKind::Alias), Some(BindingKind::Macro), None] {
            let i = raw(
                "s".to_string(),
                "n".to_string(),
                Some("a".to_string()),
                kind,
            );
            assert_eq!(i.binding_kind, kind);
            assert_eq!((i.source.as_str(), i.imported_name.as_str()), ("s", "n"));
            assert_eq!(i.alias.as_deref(), Some("a"));
        }
    }

    #[test]
    fn test_import_order_source_name_keeps_first_alias() {
        let mut imports = vec![
            import("b", "y", None),
            import("a", "x", Some("first")),
            import("a", "x", Some("second")),
        ];
        ImportOrder::SourceName.sort_dedup(&mut imports);
        let got: Vec<(&str, &str, Option<&str>)> = imports
            .iter()
            .map(|i| {
                (
                    i.source.as_str(),
                    i.imported_name.as_str(),
                    i.alias.as_deref(),
                )
            })
            .collect();
        assert_eq!(got, [("a", "x", Some("first")), ("b", "y", None)]);
    }

    #[test]
    fn test_import_order_name_source_alias_keeps_distinct_aliases() {
        let mut imports = vec![
            import("a", "x", Some("q")),
            import("a", "x", Some("p")),
            import("a", "x", Some("q")),
        ];
        ImportOrder::NameSourceAlias.sort_dedup(&mut imports);
        let aliases: Vec<Option<&str>> = imports.iter().map(|i| i.alias.as_deref()).collect();
        assert_eq!(aliases, [Some("p"), Some("q")]);
    }
}
