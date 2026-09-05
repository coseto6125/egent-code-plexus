//! `Parser::parse` has no built-in cancellation — a pathological input can
//! pin a rayon worker for seconds. tree-sitter v0.25.0's progress callback
//! lets the parser bail at the next checkpoint; this module wires it up.

use std::ops::ControlFlow;
use std::time::{Duration, Instant};
use tree_sitter::{ParseOptions, Parser, Tree};

#[derive(Clone, Copy, Debug)]
pub struct ParseBudget {
    pub max_duration: Duration,
    pub max_bytes: usize,
}

impl ParseBudget {
    pub const DEFAULT: Self = Self {
        max_duration: Duration::from_secs(1),
        max_bytes: 8 * 1024 * 1024,
    };
}

impl Default for ParseBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

pub fn parse_with_budget(parser: &mut Parser, source: &[u8], budget: ParseBudget) -> Option<Tree> {
    let start = Instant::now();
    let len = source.len();
    let mut callback = |state: &tree_sitter::ParseState| -> ControlFlow<()> {
        if state.current_byte_offset() > budget.max_bytes || start.elapsed() > budget.max_duration {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut callback);
    let tree = parser.parse_with_options(
        &mut |i, _| if i < len { &source[i..] } else { &[] },
        None,
        Some(options),
    );
    if tree.is_none() {
        // A cancelled parse leaves the parser mid-document. tree-sitter's
        // contract: "If the parser previously failed because of a timeout or a
        // cancellation, then by default, it will resume where it left off on
        // the next call to parse ... If you intend to use this parser to parse
        // some other document, you must call reset first."
        //
        // Every provider holds one `Parser` in a thread_local and reuses it for
        // every file that thread takes, so without this the file after a
        // budget-cancelled one is parsed as a continuation of the file before
        // it — a tree that belongs to neither.
        parser.reset();
    }
    tree
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `max_bytes` of zero breaks at the first checkpoint, which is the same
    /// state a real timeout leaves behind and far more reliable to arrange than
    /// a slow input. The source still has to be long enough for tree-sitter to
    /// reach a checkpoint at all — it does not poll the callback on an input it
    /// finishes in one go.
    fn cancelling_budget() -> ParseBudget {
        ParseBudget {
            max_duration: Duration::from_secs(60),
            max_bytes: 0,
        }
    }

    fn long_enough_to_checkpoint() -> Vec<u8> {
        "function alpha() { return 1; }\n"
            .repeat(200_000)
            .into_bytes()
    }

    fn js_parser() -> Parser {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_javascript::LANGUAGE.into())
            .expect("javascript grammar");
        p
    }

    /// Every provider keeps one `Parser` in a `thread_local` and reuses it for
    /// every file its thread takes. tree-sitter resumes a cancelled parse on the
    /// next call unless the parser is reset, so without the reset the file after
    /// a cancelled one is parsed as a continuation of the file before it.
    ///
    /// The assertion is on the tree, not on `is_some()`: a resumed parse can
    /// still return a tree, just one that belongs to neither document.
    #[test]
    fn a_cancelled_parse_does_not_poison_the_next_file() {
        let mut parser = js_parser();

        let first = long_enough_to_checkpoint();
        assert!(
            parse_with_budget(&mut parser, &first, cancelling_budget()).is_none(),
            "a zero-byte budget must cancel"
        );

        let second = b"function beta() { return 2; }";
        let tree = parse_with_budget(&mut parser, second, ParseBudget::DEFAULT)
            .expect("the next file must parse");
        let root = tree.root_node();
        assert_eq!(
            root.end_byte(),
            second.len(),
            "the tree must span the file it was given, not resume the previous one"
        );
        assert_eq!(
            root.utf8_text(second).unwrap(),
            std::str::from_utf8(second).unwrap(),
            "the tree must describe this file's text"
        );
        assert!(
            !root.has_error(),
            "a clean file must parse clean after a cancellation"
        );
    }
}
