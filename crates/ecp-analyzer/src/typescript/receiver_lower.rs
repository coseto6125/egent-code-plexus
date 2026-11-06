//! TypeScript lowering into the receiver core: the JavaScript lowering
//! (`javascript/receiver_lower.rs`) in its TypeScript dialect, which also
//! reads annotations, `any` / `unknown` values and method values. Unlike
//! JavaScript, a simple class annotation pins its binding.

use crate::javascript::receiver_lower::{is_function_like, lower_function, Dialect};
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use tree_sitter::Node;

pub(super) struct TsLowering;

impl Lowering for TsLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        LangPolicy {
            declared: DeclaredTypePolicy::PinnedUnlessDynamic,
            scope: ScopeRule::Block,
        }
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        is_function_like(node.kind())
    }

    fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
        lower_function(Dialect::TypeScript, function, source, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::javascript::receiver_lower::member_receiver;
    use crate::receiver::{analyze_file, Receiver};
    use std::path::Path;

    /// `(member, text)` of every `x.member` whose `x` is an identifier, in
    /// member-name order; `text` is `known T`, `unknown Cause` or `none`.
    fn receivers(source: &str) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let bytes = source.as_bytes();
        let found = analyze_file(&TsLowering, &(), tree.root_node(), bytes, Path::new("t.ts"));
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if let Some(object) = member_receiver(n) {
                let member = object.parent().unwrap();
                let name = member.child_by_field_name("property").unwrap();
                let state = match found.receiver_at(object) {
                    Some(Receiver::Known(ty)) => format!("known {}", ty.text),
                    Some(Receiver::Unknown(cause)) => format!("unknown {cause:?}"),
                    None => "none".to_string(),
                };
                out.push((name.utf8_text(bytes).unwrap().to_string(), state));
            }
            let mut cursor = n.walk();
            stack.extend(n.named_children(&mut cursor));
        }
        out.sort();
        out
    }

    fn only(source: &str) -> String {
        let mut found = receivers(source);
        assert_eq!(found.len(), 1, "{found:?}");
        found.remove(0).1
    }

    /// The state of the receiver of `member`, the one member the test asks
    /// about; other receivers in the fixture (`xs` of `xs.forEach`) may
    /// record uses of their own.
    fn of(source: &str, member: &str) -> String {
        let found = receivers(source);
        let mut rows = found.iter().filter(|(name, _)| name == member);
        let row = rows
            .next()
            .unwrap_or_else(|| panic!("no {member}: {found:?}"));
        assert!(rows.next().is_none(), "{found:?}");
        row.1.clone()
    }

    #[test]
    fn test_lower_annotated_local_is_known() {
        assert_eq!(
            only("function f() { const o: A = make(); o.m(); }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_annotated_parameter_is_known() {
        assert_eq!(only("function f(o: A) { o.m(); }"), "known A");
    }

    #[test]
    fn test_lower_construction_types_unannotated_local() {
        assert_eq!(
            only("function f() { const o = new A(); o.m(); }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_untyped_parameter_has_no_evidence() {
        assert_eq!(only("function f(o) { o.m(); }"), "unknown NoEvidence");
    }

    #[test]
    fn test_lower_any_parameter_has_no_evidence() {
        assert_eq!(only("function f(o: any) { o.m(); }"), "unknown NoEvidence");
    }

    #[test]
    fn test_lower_generic_annotation_has_no_evidence() {
        assert_eq!(
            only("function f(o: Array<A>) { o.m(); }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_block_scoped_shadow_keeps_each_type() {
        let found = receivers(
            "function f() { const o: A = a(); { const o: B = b(); o.inner(); } o.outer(); }",
        );
        assert_eq!(
            found,
            vec![
                ("inner".to_string(), "known B".to_string()),
                ("outer".to_string(), "known A".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_sibling_blocks_bind_second_block_type() {
        assert_eq!(
            only("function f() { { const o: A = a(); } { const o: B = b(); o.m(); } }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_pinned_type_survives_branch_reassignment() {
        assert_eq!(
            only("function f(c: boolean, o: A, p: A) { if (c) { o = p; } o.m(); }"),
            "known A"
        );
    }

    /// Stands for: `loose: any` is dynamic, so an element of it assigned to
    /// `obj: Cls` drops the pin inside and after the loop, while the
    /// iterable, evaluated first, keeps the outer type. A nested function
    /// that writes `obj` is a writer, whether or not it runs.
    #[test]
    fn test_lower_for_of_over_dynamic_iterable_drops_the_pin() {
        let head = "function run(xs: number[], obj: Cls, loose: any) {";
        for (body, want) in [
            (
                "for (obj of loose) { xs.forEach(obj.m); }",
                "unknown NoEvidence",
            ),
            (
                "for ([obj] of loose) { xs.forEach(obj.m); }",
                "unknown NoEvidence",
            ),
            // A loop that may run zero times joins `Cls` with the element.
            (
                "for (obj of loose) {} xs.forEach(obj.m);",
                "unknown Conflict",
            ),
            ("for (obj of xs.map(obj.m)) {}", "known Cls"),
            (
                "function* g() { for (obj of loose) {} } xs.forEach(obj.m);",
                "unknown Escape",
            ),
            (
                "const g = () => { for (obj of loose) {} }; xs.forEach(obj.m);",
                "unknown Escape",
            ),
        ] {
            assert_eq!(of(&format!("{head} {body} }}"), "m"), want, "{body}");
        }
    }

    #[test]
    fn test_lower_dynamic_cast_assignment_drops_the_pin() {
        assert_eq!(
            only("function f(p) { let o: A = new A(); o = p as any; o.m(); }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_angle_bracket_any_cast_drops_the_pin() {
        assert_eq!(
            only("function f(p) { let o: A = new A(); o = <any>p; o.m(); }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_branches_with_two_classes_conflict() {
        assert_eq!(
            only("function f(c) { let o; if (c) { o = new A(); } else { o = new B(); } o.m(); }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_lower_declaration_without_initializer_is_pinned() {
        assert_eq!(
            only("function f() { let o: A; o = g(); o.m(); }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_method_value_is_a_receiver() {
        assert_eq!(
            of("function f(xs: number[], o: A) { xs.forEach(o.m); }", "m"),
            "known A"
        );
    }

    #[test]
    fn test_lower_global_receiver_is_not_a_binding() {
        assert_eq!(only("function f() { console.log(1); }"), "none");
    }

    #[test]
    fn test_lower_closure_write_poisons_the_outer_binding() {
        assert_eq!(
            only("function f() { let o = new A(); const g = () => { o = new B(); }; o.m(); }"),
            "unknown Escape"
        );
    }

    #[test]
    fn test_lower_closure_captures_pinned_parameter() {
        assert_eq!(
            only("function f(o: A) { const g = () => o.m(); }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_inner_parameter_shadows_outer_annotation() {
        assert_eq!(
            only("function f(x: A) { const g = (x) => x.m(); }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_destructuring_rebinds_receiver() {
        assert_eq!(
            only("function f(p) { let o = new A(); ({ o } = p); o.m(); }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_empty_function_body_records_nothing() {
        assert!(receivers("function f() {}").is_empty());
    }

    #[test]
    fn test_lower_labeled_break_leaves_function_to_extractor() {
        assert_eq!(
            only("function f(xs: number[]) { const o: A = a(); outer: for (const x of xs) { break outer; } o.m(); }"),
            "none"
        );
    }

    #[test]
    fn test_lower_optional_chain_call_is_a_receiver() {
        assert_eq!(only("function f(o: A) { o?.m(); }"), "known A");
    }

    #[test]
    fn test_lower_two_calls_on_one_line_each_get_a_state() {
        let found = receivers("function f(o: A, p: B) { o.a(); p.b(); }");
        assert_eq!(
            found,
            vec![
                ("a".to_string(), "known A".to_string()),
                ("b".to_string(), "known B".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_whitespace_between_annotation_name_is_still_simple() {
        assert_eq!(only("function f(o :  A ) { o.m(); }"), "known A");
    }
}
