//! `cgn config` — interactive TUI wizard for repo-local config.
//!
//! Layout (top → bottom):
//!   1. ANSI-Shadow title `CGN FOR LLM` (static, CGN cyan → gold gradient)
//!   2. Animated 4-frame walking beaver, bouncing left↔right
//!   3. Form: Output / Confidence groups, edit-in-place
//!   4. Footer: keybinding hints
//!
//! Non-TTY environments (CI, pipes) exit with a helpful message pointing
//! at the TOML path so users can edit it manually.

use clap::Args;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use cgn_core::config::{config_path, load, save, Config};
use cgn_core::CgnError;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Terminal;
use std::io::{self, IsTerminal, Stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Args, Debug, Clone)]
pub struct ConfigArgs {
    /// Repo root. Defaults to current dir.
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(args: ConfigArgs) -> Result<(), CgnError> {
    let repo_root = args
        .repo
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    if !io::stdout().is_terminal() {
        eprintln!(
            "cgn config requires an interactive terminal.\n\
             Edit the config TOML directly: {}",
            config_path(&repo_root).display()
        );
        return Ok(());
    }

    let cfg = load(&repo_root).map_err(CgnError::InvalidArgument)?;
    let mut app = App::new(cfg, repo_root);
    let mut terminal = enter_tui().map_err(CgnError::Io)?;
    let outcome = run_loop(&mut terminal, &mut app);
    leave_tui(&mut terminal).ok();
    outcome.map_err(CgnError::Io)?;

    if app.saved {
        println!("✓ Saved to {}", config_path(&app.repo_root).display());
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Terminal lifecycle
// ─────────────────────────────────────────────────────────────────────────

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn enter_tui() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn leave_tui(terminal: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// App state
// ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum FieldId {
    OutputFormat,
    ConfidenceHighTrust,
    GroupBm25Threshold,
    GroupMaxCandidates,
    GroupCrossDepth,
    GroupTimeoutMs,
}

const FIELDS: [FieldId; 6] = [
    FieldId::OutputFormat,
    FieldId::ConfidenceHighTrust,
    FieldId::GroupBm25Threshold,
    FieldId::GroupMaxCandidates,
    FieldId::GroupCrossDepth,
    FieldId::GroupTimeoutMs,
];

struct App {
    cfg: Config,
    repo_root: PathBuf,
    cursor: usize,
    editing: bool,
    edit_buf: String,
    saved: bool,
    quit: bool,
    // Animation state
    tick: u64,
    beaver_x: i32,
    beaver_dir: i32,
}

impl App {
    fn new(cfg: Config, repo_root: PathBuf) -> Self {
        Self {
            cfg,
            repo_root,
            cursor: 0,
            editing: false,
            edit_buf: String::new(),
            saved: false,
            quit: false,
            tick: 0,
            beaver_x: 0,
            beaver_dir: 1,
        }
    }

    fn current_field(&self) -> FieldId {
        FIELDS[self.cursor]
    }

    fn field_value(&self, f: FieldId) -> String {
        match f {
            FieldId::OutputFormat => self.cfg.output.default_format.clone(),
            FieldId::ConfidenceHighTrust => {
                format!("{:.2}", self.cfg.confidence.high_trust_threshold)
            }
            FieldId::GroupBm25Threshold => format!("{:.2}", self.cfg.group.bm25_threshold),
            FieldId::GroupMaxCandidates => self.cfg.group.max_candidates_per_step.to_string(),
            FieldId::GroupCrossDepth => self.cfg.group.cross_depth.to_string(),
            FieldId::GroupTimeoutMs => self.cfg.group.local_impact_timeout_ms.to_string(),
        }
    }

    fn enter_edit(&mut self) {
        self.editing = true;
        self.edit_buf = self.field_value(self.current_field());
    }

    fn commit_edit(&mut self) {
        let v = self.edit_buf.trim().to_string();
        match self.current_field() {
            FieldId::OutputFormat => self.cfg.output.default_format = v,
            FieldId::ConfidenceHighTrust => {
                if let Ok(f) = v.parse::<f32>() {
                    self.cfg.confidence.high_trust_threshold = f.clamp(0.0, 1.0);
                }
            }
            FieldId::GroupBm25Threshold => {
                if let Ok(f) = v.parse::<f32>() {
                    self.cfg.group.bm25_threshold = f.clamp(0.0, 1.0);
                }
            }
            FieldId::GroupMaxCandidates => {
                if let Ok(n) = v.parse::<u32>() {
                    // 0 candidates breaks BM25 lookup — silent no-op cross-link generation
                    self.cfg.group.max_candidates_per_step = n.max(1);
                }
            }
            FieldId::GroupCrossDepth => {
                if let Ok(n) = v.parse::<u32>() {
                    self.cfg.group.cross_depth = n;
                }
            }
            FieldId::GroupTimeoutMs => {
                if let Ok(n) = v.parse::<u64>() {
                    self.cfg.group.local_impact_timeout_ms = n;
                }
            }
        }
        self.editing = false;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Main loop
// ─────────────────────────────────────────────────────────────────────────

fn run_loop(terminal: &mut Tui, app: &mut App) -> io::Result<()> {
    const TICK_RATE: Duration = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    while !app.quit {
        terminal.draw(|frame| render(frame, app))?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(app, key);
                }
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            tick(app);
            last_tick = Instant::now();
        }
    }
    Ok(())
}

fn tick(app: &mut App) {
    app.tick += 1;
    // Beaver x movement: 1 column per tick, flip at edges. Bounds are
    // checked against terminal width in render(); here we use a wide
    // virtual range and let the render layer clamp.
    app.beaver_x += app.beaver_dir;
    if app.beaver_x < 0 {
        app.beaver_x = 0;
        app.beaver_dir = 1;
    } else if app.beaver_x > 60 {
        app.beaver_x = 60;
        app.beaver_dir = -1;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Key handling
// ─────────────────────────────────────────────────────────────────────────

fn handle_key(app: &mut App, key: KeyEvent) {
    if app.editing {
        match key.code {
            KeyCode::Enter => app.commit_edit(),
            KeyCode::Esc => {
                app.editing = false;
                app.edit_buf.clear();
            }
            KeyCode::Backspace => {
                app.edit_buf.pop();
            }
            KeyCode::Char(c) => app.edit_buf.push(c),
            _ => {}
        }
        return;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
            app.quit = true;
        }
        (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
            if save(&app.repo_root, &app.cfg).is_ok() {
                app.saved = true;
                app.quit = true;
            }
        }
        (KeyCode::Up, _) => {
            if app.cursor > 0 {
                app.cursor -= 1;
            }
        }
        (KeyCode::Down, _) | (KeyCode::Tab, _) => {
            if app.cursor + 1 < FIELDS.len() {
                app.cursor += 1;
            }
        }
        (KeyCode::Enter, _) => app.enter_edit(),
        _ => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Rendering — palette + per-section draws
// ─────────────────────────────────────────────────────────────────────────

const CYAN: Color = Color::Rgb(0, 180, 216); // CGN primary
const GOLD: Color = Color::Rgb(255, 214, 10); // accent
const DEEP: Color = Color::Rgb(0, 119, 182); // body shadow
const DIM: Color = Color::DarkGray;

fn render(frame: &mut ratatui::Frame<'_>, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(14), // title
            Constraint::Length(7),  // beaver track
            Constraint::Min(10),    // form
            Constraint::Length(3),  // footer
        ])
        .split(frame.area());

    render_title(frame, chunks[0]);
    render_beaver(frame, chunks[1], app);
    render_form(frame, chunks[2], app);
    render_footer(frame, chunks[3], app);
}

// ----- Title -------------------------------------------------------------

const TITLE_CGN: [&str; 6] = [
    " ██████╗ ███╗   ██╗██╗  ██╗",
    "██╔════╝ ████╗  ██║╚██╗██╔╝",
    "██║  ███╗██╔██╗ ██║ ╚███╔╝ ",
    "██║   ██║██║╚██╗██║ ██╔██╗ ",
    "╚██████╔╝██║ ╚████║██╔╝ ██╗",
    " ╚═════╝ ╚═╝  ╚═══╝╚═╝  ╚═╝",
];

const TITLE_FOR: [&str; 6] = [
    "███████╗ ██████╗ ██████╗ ",
    "██╔════╝██╔═══██╗██╔══██╗",
    "█████╗  ██║   ██║██████╔╝",
    "██╔══╝  ██║   ██║██╔══██╗",
    "██║     ╚██████╔╝██║  ██║",
    "╚═╝      ╚═════╝ ╚═╝  ╚═╝",
];

const TITLE_LLM: [&str; 6] = [
    "██╗     ██╗     ███╗   ███╗",
    "██║     ██║     ████╗ ████║",
    "██║     ██║     ██╔████╔██║",
    "██║     ██║     ██║╚██╔╝██║",
    "███████╗███████╗██║ ╚═╝ ██║",
    "╚══════╝╚══════╝╚═╝     ╚═╝",
];

fn render_title(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let mut lines: Vec<Line> = Vec::with_capacity(14);
    lines.push(Line::from(""));
    for row in 0..6 {
        let spans = vec![
            Span::raw("  "),
            Span::styled(TITLE_CGN[row], Style::default().fg(CYAN)),
            Span::raw("  "),
            Span::styled(TITLE_FOR[row], Style::default().fg(GOLD)),
            Span::raw("  "),
            Span::styled(TITLE_LLM[row], Style::default().fg(DEEP)),
        ];
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(
        "  Code intelligence graph for LLMs",
        Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
    )));
    frame.render_widget(Paragraph::new(lines), area);
}

// ----- Beaver ------------------------------------------------------------

/// 4-frame walk cycle. Each frame is 5 lines tall, 7 chars wide.
const BEAVER_FRAMES: [[&str; 5]; 4] = [
    ["   ___ ", "  (·  ·)", "  /=ω=\\", "  |   |", "   \\\\  "],
    ["   ___ ", "  (·  ·)", "  /=ω=\\", "  |   /", "   //  "],
    ["   ___ ", "  (·  ·)", "  /=ω=\\", "  \\   /", "   \\\\  "],
    ["   ___ ", "  (·  ·)", "  /=ω=\\", "  \\   |", "   //  "],
];

fn render_beaver(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let frame_idx = ((app.tick / 2) % 4) as usize;
    let sprite = &BEAVER_FRAMES[frame_idx];

    let max_x = (area.width as i32 - 8).max(0);
    let x = app.beaver_x.clamp(0, max_x) as usize;

    let mut lines: Vec<Line> = Vec::with_capacity(5);
    for (row_idx, row) in sprite.iter().enumerate() {
        let colored = colorize_beaver_row(row, row_idx);
        let mut spans = vec![Span::raw(" ".repeat(x))];
        spans.extend(colored);
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Paint each char of a beaver-sprite row with CGN palette per role:
/// teeth (`ω`) → gold; eyes (`·`) → white; legs/tail (rows 3-4) → deep blue;
/// rest → cyan.
fn colorize_beaver_row(row: &str, row_idx: usize) -> Vec<Span<'static>> {
    let body_color = if row_idx >= 3 { DEEP } else { CYAN };
    row.chars()
        .map(|c| {
            let color = match c {
                'ω' => GOLD,
                '·' => Color::White,
                _ if c.is_whitespace() => return Span::raw(c.to_string()),
                _ => body_color,
            };
            Span::styled(c.to_string(), Style::default().fg(color))
        })
        .collect()
}

// ----- Form --------------------------------------------------------------

fn render_form(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CYAN))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Configuration",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(
                config_path(&app.repo_root).display().to_string(),
                Style::default().fg(DIM),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = vec![
        group_header("Output"),
        field_line(app, FieldId::OutputFormat, "Default format"),
        Line::from(""),
        group_header("Confidence"),
        field_line(app, FieldId::ConfidenceHighTrust, "High-trust thr."),
        Line::from(""),
        // TODO: expose exclude_links_paths (Vec<String>) and exclude_links_param_only_paths (bool) when TUI gains list/toggle widgets
        group_header("Group"),
        field_line(app, FieldId::GroupBm25Threshold, "BM25 threshold"),
        field_line(app, FieldId::GroupMaxCandidates, "Max candidates"),
        field_line(app, FieldId::GroupCrossDepth, "Cross depth"),
        field_line(app, FieldId::GroupTimeoutMs, "Impact timeout ms"),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
}

fn group_header(name: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            name.to_string(),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn field_line(app: &App, field: FieldId, label: &str) -> Line<'static> {
    let focused = app.current_field() == field;
    let editing = focused && app.editing;
    let cursor = if focused {
        Span::styled(
            "  ▶  ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("     ")
    };
    let label_span = Span::styled(format!("{:<20}", label), Style::default().fg(Color::White));
    let value_text = if editing {
        format!("{}_", app.edit_buf)
    } else {
        app.field_value(field)
    };
    let value_color = if editing { GOLD } else { Color::White };
    let value = Span::styled(
        format!("{:<32}", truncate(&value_text, 32)),
        Style::default().fg(value_color),
    );
    let status = Span::styled("[⚠ stored, pending]", Style::default().fg(Color::Yellow));
    Line::from(vec![cursor, label_span, value, status])
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

// ----- Footer ------------------------------------------------------------

fn render_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let hints: Vec<Span> = if app.editing {
        vec![
            kbd("Enter"),
            Span::raw(" commit   "),
            kbd("Esc"),
            Span::raw(" cancel   "),
            kbd("⌫"),
            Span::raw(" delete"),
        ]
    } else {
        vec![
            kbd("↑/↓"),
            Span::raw(" navigate   "),
            kbd("Enter"),
            Span::raw(" edit   "),
            kbd("^S"),
            Span::raw(" save   "),
            kbd("^C/q"),
            Span::raw(" quit"),
        ]
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(DIM));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(Line::from(hints)), inner);
}

fn kbd(s: &str) -> Span<'static> {
    Span::styled(
        format!(" {s} "),
        Style::default()
            .fg(Color::Black)
            .bg(CYAN)
            .add_modifier(Modifier::BOLD),
    )
}

// ─────────────────────────────────────────────────────────────────────────
// Snapshot helper (test-only) — renders a single frame into a string
// painted with ANSI escapes so the test harness can dump it to stderr
// and show the user what `cgn config` will look like without launching
// the full TUI loop.
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub fn snapshot_ansi(width: u16, height: u16, repo_root: PathBuf, tick: u64, x: i32) -> String {
    use ratatui::backend::TestBackend;
    let app = App {
        cfg: Config::default(),
        repo_root,
        cursor: 0,
        editing: false,
        edit_buf: String::new(),
        saved: false,
        quit: false,
        tick,
        beaver_x: x,
        beaver_dir: 1,
    };
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| render(f, &app)).unwrap();
    buffer_to_ansi(terminal.backend().buffer())
}

#[cfg(test)]
fn buffer_to_ansi(buf: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    let area = buf.area;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let style = cell.style();
            out.push_str(&style_to_ansi(style));
            out.push_str(cell.symbol());
            out.push_str("\x1b[0m");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
fn style_to_ansi(style: Style) -> String {
    let mut out = String::new();
    if let Some(Color::Rgb(r, g, b)) = style.fg {
        out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
    } else if let Some(c) = style.fg {
        let code = match c {
            Color::Black => 30,
            Color::Red => 31,
            Color::Green => 32,
            Color::Yellow => 33,
            Color::Blue => 34,
            Color::Magenta => 35,
            Color::Cyan => 36,
            Color::White => 37,
            Color::DarkGray => 90,
            _ => 39,
        };
        out.push_str(&format!("\x1b[{code}m"));
    }
    if let Some(Color::Rgb(r, g, b)) = style.bg {
        out.push_str(&format!("\x1b[48;2;{r};{g};{b}m"));
    }
    if style.add_modifier.contains(Modifier::BOLD) {
        out.push_str("\x1b[1m");
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        out.push_str("\x1b[3m");
    }
    out
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    /// Render one frame at 100×42 and print it. Run with:
    ///     cargo test -p code-graph-nexus --lib config::snapshot_tests -- --nocapture
    #[test]
    fn print_snapshot() {
        let out = snapshot_ansi(100, 42, PathBuf::from("."), 0, 30);
        eprintln!("\n{out}");
    }

    /// Plain-text layout dump (no colors) — easier to read in pipelines /
    /// logs / GitHub PR review threads.
    #[test]
    fn print_snapshot_plain() {
        use ratatui::backend::TestBackend;
        let app = App {
            cfg: Config::default(),
            repo_root: PathBuf::from("."),
            cursor: 0,
            editing: false,
            edit_buf: String::new(),
            saved: false,
            quit: false,
            tick: 0,
            beaver_x: 30,
            beaver_dir: 1,
        };
        let backend = TestBackend::new(100, 42);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        eprintln!("\n{out}");
    }
}
