use super::gain::{GainArgs, Rec};
use crate::output::OutputFormat;

pub fn color_enabled(_a: &GainArgs, _f: &OutputFormat) -> bool {
    false
}
pub fn render_dashboard(_r: &[Rec], _c: bool, _all: bool) -> String {
    String::new()
}
pub fn render_failures(_r: &[Rec], _c: bool) -> String {
    String::new()
}
