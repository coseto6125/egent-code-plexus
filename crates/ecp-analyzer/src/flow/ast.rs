use std::collections::BTreeMap;
use tree_sitter::Node;

#[derive(Clone)]
pub(super) struct Ast {
    pub(super) parent: Option<usize>,
    pub(super) size: usize,
    pub(super) operator: Option<String>,
    pub(super) kind: String,
    pub(super) text: String,
    pub(super) file: usize,
    pub(super) line: usize,
    pub(super) column: usize,
    pub(super) end_line: usize,
    pub(super) end_column: usize,
    pub(super) children: Vec<usize>,
    pub(super) fields: BTreeMap<String, usize>,
}
/// `fields_of[id]` records the field name each node was pushed under, so a
/// caller that cuts the vector at a budget can rebuild parents' field maps
/// exactly as a lowering that stopped at that node would have left them.
pub(super) fn lower(
    n: Node<'_>,
    file: usize,
    source: &str,
    ast: &mut Vec<Ast>,
    fields_of: &mut Vec<Option<&'static str>>,
    limit: usize,
) -> (usize, bool) {
    let root = ast.len();
    let mut pending = vec![(n, None, None, 0usize)];
    let mut truncated = false;
    while let Some((n, parent, field, depth)) = pending.pop() {
        if parent.is_some() && (ast.len() >= limit || depth >= 192) {
            truncated = true;
            continue;
        }
        let id = ast.len();
        let semantic_text = matches!(
            n.kind(),
            "identifier"
                | "name"
                | "variable_name"
                | "property_identifier"
                | "dotted_name"
                | "relative_import"
        ) || (matches!(n.kind(), "string" | "integer" | "number")
            && (matches!(field, Some("source" | "module_name" | "key"))
                || parent.is_some_and(|parent: usize| {
                    matches!(
                        ast[parent].kind.as_str(),
                        "subscript_expression" | "member_expression" | "attribute"
                    )
                })));
        let text = if semantic_text {
            source[n.byte_range()].to_string()
        } else {
            source[n.byte_range()].chars().take(256).collect()
        };
        ast.push(Ast {
            parent,
            size: n.end_byte() - n.start_byte(),
            operator: n
                .child_by_field_name("operator")
                .map(|op| source[op.byte_range()].to_string()),
            kind: n.kind().into(),
            text,
            file,
            line: n.start_position().row + 1,
            column: n.start_position().column + 1,
            end_line: n.end_position().row + 1,
            end_column: n.end_position().column + 1,
            children: vec![],
            fields: BTreeMap::new(),
        });
        fields_of.push(field);
        if let Some(parent) = parent {
            ast[parent].children.push(id);
            if let Some(field) = field {
                ast[parent].fields.insert(field.into(), id);
            }
        }
        for i in (0..n.child_count()).rev() {
            let child = n.child(i as u32).unwrap();
            if child.is_named() {
                pending.push((child, Some(id), n.field_name_for_child(i as u32), depth + 1));
            }
        }
    }
    (root, truncated)
}
