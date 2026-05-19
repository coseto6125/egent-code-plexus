//! Verilog identifier finder. tree-sitter-verilog uses several
//! kind-specific identifier nodes — covered the four common ones.

use super::generic::find_by_kinds;
use cgn_core::analyzer::types::IdentifierRange;

const KINDS: &[&str] = &[
    "simple_identifier",
    "function_identifier",
    "task_identifier",
    "parameter_identifier",
];

pub fn find_identifier_occurrences(source: &[u8], target_name: &str) -> Vec<IdentifierRange> {
    find_by_kinds(
        source,
        target_name,
        &tree_sitter_verilog::LANGUAGE.into(),
        KINDS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_module_and_instance() {
        let src = b"module foo(input a, output b); endmodule\nmodule top; foo u1(.a(1), .b()); endmodule\n";
        let hits = find_identifier_occurrences(src, "foo");
        assert!(hits.len() >= 2, "{:?}", hits);
    }
}
