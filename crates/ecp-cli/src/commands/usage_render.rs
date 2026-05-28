//! ASCII rendering for `ecp usage`. Sections: Usage → Performance → Errors.
//! Color is opt-in and auto-disabled off a TTY / under NO_COLOR / for json.

use super::usage::{by_command, errors_by_kind, percentile, Rec, UsageArgs};
use crate::output::OutputFormat;
use ecp_core::time::unix_secs_to_rfc3339;
use std::fmt::Write as _;
use std::io::IsTerminal;

const BUDGET_MS: u64 = 30;
const BAR_W: usize = 12;

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// Three-state rule: --no-color/NO_COLOR off; non-text format off; non-TTY off; else on.
pub fn color_enabled(args: &UsageArgs, format: &OutputFormat) -> bool {
    if args.no_color || std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if !matches!(format, OutputFormat::Text) {
        return false;
    }
    std::io::stdout().is_terminal()
}

fn paint(s: &str, color: &str, on: bool) -> String {
    if on {
        format!("{color}{s}{RESET}")
    } else {
        s.to_string()
    }
}

fn bar(frac: f64, width: usize) -> String {
    let filled = ((frac * width as f64).round() as usize).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

pub fn render_dashboard(recs: &[Rec], color: bool, show_all: bool) -> String {
    let mut o = String::new();
    let total = recs.len();
    if total == 0 {
        return "ecp Usage Dashboard\n  (no telemetry yet — run some ecp commands first)"
            .to_string();
    }
    let errors = recs.iter().filter(|r| !r.ok).count();
    let within = recs.iter().filter(|r| r.duration_ms <= BUDGET_MS).count();
    let cli_n = recs.iter().filter(|r| r.source == "cli").count();
    let mcp_n = total - cli_n;
    let mut all_durs: Vec<u64> = recs.iter().map(|r| r.duration_ms).collect();
    all_durs.sort_unstable();
    let p50 = percentile(&all_durs, 50);
    let p99 = percentile(&all_durs, 99);
    let err_pct = errors as f64 / total as f64 * 100.0;

    let _ = writeln!(o, "ecp Usage Dashboard");
    let _ = writeln!(o, "{}", "═".repeat(76));
    let _ = writeln!(
        o,
        " Total invocations  {total:<10}  Error rate  {err_pct:.1}%  ({errors} failed)"
    );
    let _ = writeln!(
        o,
        " Median latency     {p50:<3}ms      p99 {p99}ms     Sources  cli {cli_n} · mcp {mcp_n}"
    );
    let _ = writeln!(o);

    let _ = writeln!(o, "▌Usage  (by subcommand)");
    let _ = writeln!(o, "{}", "─".repeat(76));
    let _ = writeln!(
        o,
        "  {:<2} {:<22} {:>6}  {:>6}  {:>5}  {:>6}  Err%",
        "#", "Command", "Count", "Share", "p50", "p99"
    );
    let _ = writeln!(o, "{}", "─".repeat(76));
    let stats = by_command(recs);
    let max_count = stats.first().map(|s| s.count).unwrap_or(1).max(1);
    let shown = if show_all {
        stats.len()
    } else {
        stats.len().min(10)
    };
    for (i, s) in stats.iter().take(shown).enumerate() {
        let share = s.count as f64 / total as f64 * 100.0;
        let er = if s.count > 0 {
            s.errors as f64 / s.count as f64 * 100.0
        } else {
            0.0
        };
        let er_cell = if er >= 5.0 {
            paint(&format!("{er:>4.1}% !"), RED, color)
        } else {
            format!("{er:>4.1}%  ")
        };
        let b = bar(s.count as f64 / max_count as f64, BAR_W);
        let _ = writeln!(
            o,
            "  {:<2} {:<22} {:>6}  {:>6}  {:>5}  {:>6}  {}  {}",
            i + 1,
            s.cmd,
            s.count,
            format!("{share:.1}%"),
            format!("{}ms", s.p50),
            format!("{}ms", s.p99),
            er_cell,
            b
        );
    }
    if !show_all && stats.len() > shown {
        let _ = writeln!(o, "  …  ({} more — see --all)", stats.len() - shown);
    }
    let _ = writeln!(o, "{}", "─".repeat(76));
    let _ = writeln!(o);

    let _ = writeln!(o, "▌Performance  (latency budget: <{BUDGET_MS}ms target)");
    let _ = writeln!(o, "{}", "─".repeat(76));
    let within_frac = within as f64 / total as f64;
    let over = total - within;
    let within_lbl = paint("Within budget", GREEN, color);
    let over_lbl = paint(
        "Over budget  ",
        if within_frac < 0.7 { RED } else { YELLOW },
        color,
    );
    let _ = writeln!(
        o,
        "  {within_lbl}  {}  {:.1}%  ({within})",
        bar(within_frac, 24),
        within_frac * 100.0
    );
    let _ = writeln!(
        o,
        "  {over_lbl}  {}  {:.1}%  ({over})",
        bar(1.0 - within_frac, 24),
        (1.0 - within_frac) * 100.0
    );
    let _ = writeln!(o, "{}", "─".repeat(76));
    let _ = writeln!(o);

    let _ = writeln!(o, "▌Errors  ({errors} total · {err_pct:.1}%)");
    let _ = writeln!(o, "{}", "─".repeat(76));
    let ebk = errors_by_kind(recs);
    let max_e = ebk.values().copied().max().unwrap_or(1).max(1);
    if ebk.is_empty() {
        let _ = writeln!(o, "  (none)");
    } else {
        let mut pairs: Vec<(String, usize)> = ebk.into_iter().collect();
        pairs.sort_by_key(|p| std::cmp::Reverse(p.1));
        for (kind, n) in pairs {
            let _ = writeln!(
                o,
                "  {:<20} {:>4}  {}",
                kind,
                n,
                bar(n as f64 / max_e as f64, 16)
            );
        }
        let _ = writeln!(
            o,
            "  Tip: ecp usage --failures   for recent failing commands + messages"
        );
    }
    let _ = writeln!(o, "{}", "─".repeat(76));
    o
}

pub fn render_failures(recs: &[Rec], _color: bool) -> String {
    let fails: Vec<&Rec> = recs.iter().filter(|r| !r.ok).collect();
    let mut o = String::new();
    let _ = writeln!(
        o,
        "ecp Failures  (recent {} of {})",
        fails.len().min(20),
        fails.len()
    );
    let _ = writeln!(o, "{}", "═".repeat(76));
    // Header row: tool column widens to fit "tool sub" for v2 records.
    for r in fails.iter().rev().take(20) {
        let kind = r.error_kind.as_deref().unwrap_or("other");
        let verb = match &r.subcommand {
            Some(s) => format!("{} {}", r.tool, s),
            None => r.tool.clone(),
        };
        let _ = writeln!(
            o,
            "  {}  {:<22} {}",
            unix_secs_to_rfc3339(r.ts_secs),
            verb,
            kind
        );
        // Prefer the v2 `error_msg` field (already sanitized + capped at 200
        // bytes by the writer); fall back to the raw jsonl line for pre-v2
        // records. No further truncation — the writer is the canonical
        // truncation point so the reader stays losslessly diagnostic.
        let detail = r.error_msg.as_deref().unwrap_or(&r.raw);
        let _ = writeln!(o, "       └ {detail}");
    }
    let _ = writeln!(o, "{}", "─".repeat(76));
    o
}
