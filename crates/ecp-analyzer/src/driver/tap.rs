//! The record tap: the main query pass records the captures that the
//! schema-field configs name, so `Op::SchemaFields` builds the records
//! without a second query run. It keeps `extract_schema_fields`'s rules:
//! its gate, its required owner and name slots, and at most one record per
//! match, from the first active config.

use super::{node, FileCx};
use crate::framework_helpers::has_import_from;
use crate::schema_field::extract::schema_record;
use crate::schema_field::SchemaFieldConfig;
use ecp_core::analyzer::types::Span;
use tree_sitter::{Node, Query, QueryCapture};

/// Schema-field configs per tapping spec; TypeScript, the only one, has 1.
/// The per-match slot table is a fixed array of this size, so a file
/// allocates no slots.
pub(super) const MAX_CONFIGS: usize = 8;

type ByteRange = (u32, u32);
/// `[owner, name, type]`.
type Slots = [Option<ByteRange>; 3];

#[derive(Clone, Copy)]
struct TapTarget {
    cfg: u8,
    role: u8,
}

/// One capture index's targets: `TapTable::targets[start..start + len]`.
#[derive(Clone, Copy, Default)]
pub(super) struct TapRange {
    start: u16,
    len: u16,
}

impl TapRange {
    pub(super) fn is_empty(self) -> bool {
        self.len == 0
    }
}

pub(super) struct TapTable {
    targets: Box<[TapTarget]>,
    configs: u8,
}

impl TapTable {
    /// Returns the table and one range per capture index of `query`. Every
    /// config gets targets, active or not: the gates read imports that the
    /// query pass itself collects.
    pub(super) fn new(query: &Query, configs: &[SchemaFieldConfig]) -> (Self, Vec<TapRange>) {
        assert!(
            configs.len() <= MAX_CONFIGS,
            "{} schema configs exceed the tap's {MAX_CONFIGS}",
            configs.len()
        );
        let mut per_capture = vec![Vec::new(); query.capture_names().len()];
        for (cfg, c) in configs.iter().enumerate() {
            let roles = [c.owner_capture, c.name_capture, c.type_capture];
            for (role, name) in roles.into_iter().enumerate() {
                // An empty name has no role in the extractor.
                if name.is_empty() {
                    continue;
                }
                if let Some(index) = query.capture_index_for_name(name) {
                    per_capture[index as usize].push(TapTarget {
                        cfg: cfg as u8,
                        role: role as u8,
                    });
                }
            }
        }
        let mut targets = Vec::new();
        let ranges: Vec<TapRange> = per_capture
            .into_iter()
            .map(|list: Vec<TapTarget>| {
                let range = TapRange {
                    start: targets.len() as u16,
                    len: list.len() as u16,
                };
                targets.extend(list);
                range
            })
            .collect();
        let table = Self {
            targets: targets.into_boxed_slice(),
            configs: configs.len() as u8,
        };
        (table, ranges)
    }
}

/// One tapped match's candidate for one config. Ranges, not strings: the
/// gates decide after the query pass whether any text is decoded.
#[derive(Clone, Copy)]
pub(super) struct TapRow {
    /// The ordinal of the tapped match; equal `seq` rows form one group.
    seq: u32,
    cfg: u8,
    span: Span,
    slots: Slots,
}

const _: () = assert!(std::mem::size_of::<TapRow>() <= 64);

/// The per-match slot table, reused across one file's matches.
#[derive(Default)]
pub(super) struct TapMatch {
    slots: [Slots; MAX_CONFIGS],
    dirty: bool,
    seq: u32,
}

impl TapMatch {
    /// A repeated capture overwrites its slot: the last write wins.
    #[inline]
    pub(super) fn write(&mut self, t: &TapTable, range: TapRange, node: Node<'_>) {
        // tree-sitter stores byte offsets as u32, so the casts are exact.
        let bytes = (node.start_byte() as u32, node.end_byte() as u32);
        let start = range.start as usize;
        for target in &t.targets[start..start + range.len as usize] {
            self.slots[target.cfg as usize][target.role as usize] = Some(bytes);
        }
        self.dirty = true;
    }

    /// Ends a match: when it wrote a slot, keeps one row per config whose
    /// owner and name slots are set, then clears the slots.
    #[inline]
    pub(super) fn close(
        &mut self,
        t: &TapTable,
        captures: &[QueryCapture<'_>],
        rows: &mut Vec<TapRow>,
    ) {
        if self.dirty {
            self.flush(t, captures[0].node, rows);
        }
    }

    fn flush(&mut self, t: &TapTable, first: Node<'_>, rows: &mut Vec<TapRow>) {
        let span = node::span(first);
        let configs = t.configs as usize;
        let seq = self.seq;
        let candidates = self.slots[..configs].iter().enumerate();
        rows.extend(
            candidates
                .filter(|(_, s)| s[0].is_some() && s[1].is_some())
                .map(|(cfg, slots)| TapRow {
                    seq,
                    cfg: cfg as u8,
                    span,
                    slots: *slots,
                }),
        );
        self.slots[..configs].fill([None; 3]);
        self.dirty = false;
        self.seq += 1;
    }
}

/// `Op::SchemaFields`: the first row of each match group whose config is
/// active. An empty gate is always active (TS interfaces). The rows drop
/// with this step, so none outlives it.
pub(super) fn schema_fields(configs: &[SchemaFieldConfig], cx: &mut FileCx<'_>) {
    let rows = std::mem::take(&mut cx.tap_rows);
    let mut on = [false; MAX_CONFIGS];
    for (flag, c) in on.iter_mut().zip(configs) {
        *flag = c.import_gate.is_empty() || has_import_from(&cx.imports, c.import_gate);
    }
    let source = cx.source;
    // `Node::utf8_text` with its error mapped as the extractor maps it.
    let text = |slot: Option<ByteRange>| {
        slot.map(|(a, b)| std::str::from_utf8(&source[a as usize..b as usize]).unwrap_or(""))
    };
    let mut out = Vec::new();
    let mut emitted = None;
    for row in &rows {
        if emitted == Some(row.seq) || !on[row.cfg as usize] {
            continue;
        }
        emitted = Some(row.seq);
        let [owner, name, type_text] = row.slots.map(text);
        // A row exists only with its owner and name slots set.
        out.push(schema_record(
            &configs[row.cfg as usize],
            owner.unwrap_or_default(),
            name.unwrap_or_default(),
            type_text,
            row.span,
        ));
    }
    cx.schema_fields = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::node::rust_tree;
    use crate::driver::{parse, Compiled, ParseSpec, TestRule};
    use crate::python::schema_extractors::{PYDANTIC_CONFIG, SQLALCHEMY_CONFIG};
    use crate::python::spec::PythonSpec;
    use crate::schema_field::{classify_ts_type, extract_schema_fields};
    use crate::typescript::schema_extractors::TS_INTERFACE_CONFIG;
    use crate::typescript::spec::TypeScriptSpec;
    use ecp_core::analyzer::types::{FrameworkId, RawImport, RawSchemaField};
    use std::path::Path;
    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Language, Parser, QueryCursor, Tree};

    /// The tap fed by a plain `QueryCursor` loop, as the driver feeds it.
    fn tap_pass(
        query: &Query,
        configs: &[SchemaFieldConfig],
        tree: &Tree,
        src: &[u8],
    ) -> Vec<TapRow> {
        let (table, ranges) = TapTable::new(query, configs);
        let (mut tap, mut rows) = (TapMatch::default(), Vec::new());
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), src);
        while let Some(m) = matches.next() {
            for cap in m.captures() {
                let range = ranges[cap.index as usize];
                if !range.is_empty() {
                    tap.write(&table, range, cap.node);
                }
            }
            tap.close(&table, m.captures(), &mut rows);
        }
        rows
    }

    fn import(source: &str) -> RawImport {
        RawImport {
            source: source.to_string(),
            imported_name: "*".to_string(),
            alias: None,
            binding_kind: None,
        }
    }

    /// Runs the schema step on `cx` and checks it against the extractor.
    fn schemas_both_ways(
        query: &Query,
        configs: &[SchemaFieldConfig],
        cx: &mut FileCx<'_>,
        imports: &[RawImport],
    ) -> Vec<RawSchemaField> {
        cx.imports = imports.to_vec();
        cx.tap_rows = tap_pass(query, configs, cx.tree, cx.source);
        schema_fields(configs, cx);
        let want = extract_schema_fields(cx.tree, cx.source, query, configs, imports);
        assert_eq!(format!("{:?}", cx.schema_fields), format!("{want:?}"));
        want
    }

    fn new_cx<'t>(tree: &'t Tree, src: &'t [u8]) -> FileCx<'t> {
        FileCx::new(tree, src, Path::new("t"), TestRule::Category)
    }

    fn rust_query(text: &str) -> Query {
        Query::new(&tree_sitter_rust::LANGUAGE.into(), text).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Owner `o`, name `n`, no type, behind `gate`.
    const fn schema(framework: FrameworkId, gate: &'static [&'static str]) -> SchemaFieldConfig {
        SchemaFieldConfig {
            framework,
            owner_capture: "o",
            name_capture: "n",
            type_capture: "",
            import_gate: gate,
            ..TS_INTERFACE_CONFIG
        }
    }

    fn tree_for(language: &Language, src: &[u8]) -> Tree {
        let mut parser = Parser::new();
        parser.set_language(language).expect("language");
        parser.parse(src, None).expect("tree")
    }

    fn python() -> (Language, Query) {
        let language: Language = tree_sitter_python::LANGUAGE.into();
        let query = PythonSpec::query().unwrap_or_else(|e| panic!("{e}"));
        (language, query)
    }

    const PYTHON_CONFIGS: [SchemaFieldConfig; 2] = [PYDANTIC_CONFIG, SQLALCHEMY_CONFIG];

    #[test]
    fn test_schema_tap_typescript_driver_equals_extract_schema_fields() {
        let src = "interface User { name: string; age: number; }\n";
        let c = Compiled::<TypeScriptSpec>::new().unwrap_or_else(|e| panic!("{e}"));
        let g = parse(&c, Path::new("user.ts"), src.as_bytes()).unwrap_or_else(|e| panic!("{e}"));
        let tree = TypeScriptSpec::parse_tree(src.as_bytes()).expect("tree");
        let want = extract_schema_fields(
            &tree,
            src.as_bytes(),
            c.query(),
            TypeScriptSpec::SCHEMA_FIELDS,
            &g.imports,
        );
        let got = g.schema_fields.as_deref().unwrap_or_default();
        assert_eq!(format!("{got:?}"), format!("{want:?}"));
        let fields: Vec<_> = got.iter().map(|f| (&*f.owner_class, &*f.name)).collect();
        assert_eq!(fields, [("User", "name"), ("User", "age")]);
    }

    #[test]
    fn test_schema_tap_ts_interface_empty_gate_is_active() {
        let language: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let query = TypeScriptSpec::query().unwrap_or_else(|e| panic!("{e}"));
        let src = b"interface User { name: string; age: number; }\n";
        let tree = tree_for(&language, src);
        assert!(TS_INTERFACE_CONFIG.import_gate.is_empty());
        let got = schemas_both_ways(&query, &[TS_INTERFACE_CONFIG], &mut new_cx(&tree, src), &[]);
        let fields: Vec<_> = got.iter().map(|f| (&*f.owner_class, &*f.name)).collect();
        assert_eq!(fields, [("User", "name"), ("User", "age")]);
        assert_eq!(got[0].type_class, classify_ts_type("string"));
    }

    #[test]
    fn test_schema_tap_pydantic_equals_extract_schema_fields() {
        let (language, query) = python();
        let src = b"from pydantic import BaseModel\n\nclass User(BaseModel):\n    name: str\n    age: int\n";
        let tree = tree_for(&language, src);
        let imports = [import("pydantic")];
        let got = schemas_both_ways(&query, &PYTHON_CONFIGS, &mut new_cx(&tree, src), &imports);
        let names: Vec<_> = got.iter().map(|f| (f.framework, &*f.name)).collect();
        assert_eq!(
            names,
            [
                (FrameworkId::Pydantic, "name"),
                (FrameworkId::Pydantic, "age")
            ]
        );
    }

    #[test]
    fn test_schema_tap_sqlalchemy_equals_extract_schema_fields() {
        let (language, query) = python();
        let src = b"from sqlalchemy import Column, String\n\nclass User(Base):\n    name = Column(String)\n";
        let tree = tree_for(&language, src);
        let imports = [import("sqlalchemy")];
        let got = schemas_both_ways(&query, &PYTHON_CONFIGS, &mut new_cx(&tree, src), &imports);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].framework, FrameworkId::SqlAlchemy);
        assert_eq!((&*got[0].owner_class, &*got[0].name), ("User", "name"));
    }

    #[test]
    fn test_schema_tap_two_configs_one_match_emits_once() {
        let src = b"struct A { x: u8 }\nstruct B { y: u8 }\n";
        let tree = rust_tree(src);
        let query = rust_query(
            "(struct_item name: (type_identifier) @o
                body: (field_declaration_list
                    (field_declaration name: (field_identifier) @n)))",
        );
        let configs = [
            schema(FrameworkId::Pydantic, &["first"]),
            schema(FrameworkId::SqlAlchemy, &["second"]),
        ];
        let fields = |got: Vec<RawSchemaField>| -> Vec<(FrameworkId, String)> {
            got.iter()
                .map(|f| (f.framework, format!("{}.{}", f.owner_class, f.name)))
                .collect()
        };
        let both = [import("first"), import("second")];
        let got = schemas_both_ways(&query, &configs, &mut new_cx(&tree, src), &both);
        assert_eq!(
            fields(got),
            [
                (FrameworkId::Pydantic, "A.x".to_string()),
                (FrameworkId::Pydantic, "B.y".to_string())
            ]
        );
        let second = [import("second")];
        let got = schemas_both_ways(&query, &configs, &mut new_cx(&tree, src), &second);
        assert_eq!(
            fields(got),
            [
                (FrameworkId::SqlAlchemy, "A.x".to_string()),
                (FrameworkId::SqlAlchemy, "B.y".to_string())
            ]
        );
        let got = schemas_both_ways(&query, &configs, &mut new_cx(&tree, src), &[]);
        assert!(got.is_empty());
        let empty = rust_tree(b"");
        let got = schemas_both_ways(&query, &configs, &mut new_cx(&empty, b""), &both);
        assert!(got.is_empty());
    }

    #[test]
    fn test_schema_tap_repeated_capture_last_write_wins() {
        let src = b"struct A { x: u8 }\n";
        let tree = rust_tree(src);
        let query = rust_query(
            "(field_declaration name: (field_identifier) @n
                type: (primitive_type) @n) @o",
        );
        let configs = [schema(FrameworkId::Pydantic, &[])];
        let got = schemas_both_ways(&query, &configs, &mut new_cx(&tree, src), &[]);
        let names: Vec<_> = got.iter().map(|f| &*f.name).collect();
        assert_eq!(names, ["u8"]);
    }

    #[test]
    fn test_schema_tap_invalid_utf8_name_emits_empty_name() {
        let src = b"fn a() { f(\"\xff\"); }\n";
        let tree = rust_tree(src);
        let query = rust_query(
            "(call_expression function: (identifier) @o
                arguments: (arguments (string_literal) @n))",
        );
        let configs = [schema(FrameworkId::Pydantic, &[])];
        let got = schemas_both_ways(&query, &configs, &mut new_cx(&tree, src), &[]);
        let fields: Vec<_> = got.iter().map(|f| (&*f.owner_class, &*f.name)).collect();
        assert_eq!(fields, [("f", "")]);
    }

    #[test]
    fn test_schema_tap_owner_without_name_emits_nothing() {
        let src = b"struct A { x: u8 }\n";
        let tree = rust_tree(src);
        let query = rust_query(
            "(struct_item name: (type_identifier) @o)
             (field_declaration name: (field_identifier) @n)",
        );
        let configs = [schema(FrameworkId::Pydantic, &[])];
        assert!(tap_pass(&query, &configs, &tree, src).is_empty());
        let got = schemas_both_ways(&query, &configs, &mut new_cx(&tree, src), &[]);
        assert!(got.is_empty());
    }

    #[test]
    fn test_schema_tap_python_no_import_returns_none() {
        let (language, query) = python();
        let src = b"from pydantic import BaseModel\n\nclass User(BaseModel):\n    name: str\n";
        let tree = tree_for(&language, src);
        let rows = tap_pass(&query, &PYTHON_CONFIGS, &tree, src);
        assert!(!rows.is_empty(), "the fields must be tapped");
        let mut cx = new_cx(&tree, src);
        let got = schemas_both_ways(&query, &PYTHON_CONFIGS, &mut cx, &[]);
        assert!(got.is_empty());
        assert_eq!(cx.tap_rows.capacity(), 0, "the rows outlived the step");
        assert!(cx.into_graph().schema_fields.is_none());
    }
}
