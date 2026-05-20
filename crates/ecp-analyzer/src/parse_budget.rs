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
    parser.parse_with_options(
        &mut |i, _| if i < len { &source[i..] } else { &[] },
        None,
        Some(options),
    )
}
