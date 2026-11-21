//! `settings` — the configuration surface, in the Aivyx palette.
//!
//! Aivyx resolves config with a precedence chain — environment ›
//! `aivyx-pa.toml` › built-in defaults — and can report exactly where
//! each value came from (the `aivyx-pa config sources` output). This
//! screen leans on that: a categories column on the left, and on the
//! right the selected category's settings, each annotated with its
//! **source** `(env)` / `(toml)` / `(default)`. The categories mirror
//! the real config sections (provider, daemon, storage/sandbox,
//! memory/recall, self-learning, loop, notifications, security).
//!
//! Run it live (truecolor, any key to exit):
//!     cargo run -p aivyx-tui --example settings
//! Or print the character grid:
//!     cargo run -p aivyx-tui --example settings -- --snapshot

use std::io::{self, IsTerminal};
use std::time::Duration;

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph};
use ratatui::{Frame, Terminal};

use aivyx_tui::palette::{AMBER, BG, BORDER, DIM, DIMMER, FG, LAV, OK, STATUS_BG};

const W: u16 = 104;
const H: u16 = 36;
const SEL_CAT: usize = 0; // Model & Provider

fn dim() -> Style {
    Style::default().fg(DIM)
}
fn bold(c: Color) -> Style {
    Style::default().fg(c).add_modifier(Modifier::BOLD)
}

fn panel(title: &str, focused: bool) -> Block<'_> {
    let (bc, tc) = if focused { (AMBER, AMBER) } else { (BORDER, DIM) };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(bc))
        .title(Span::styled(format!(" {title} "), bold(tc)))
        .padding(Padding::new(1, 1, 0, 0))
        .style(Style::default().bg(BG))
}

/// A `label   value … (source)` settings row.
fn row(label: &'static str, value: Vec<Span<'static>>, source: &'static str) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:<12}"), dim())];
    spans.extend(value);
    spans.push(Span::styled(format!("   ({source})"), Style::default().fg(DIMMER)));
    Line::from(spans)
}

fn draw(f: &mut Frame) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::default().bg(BG)), area);

    let rows = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Length(1), // precedence line
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // body
        Constraint::Length(1), // status
    ])
    .split(area);

    header(f, rows[0]);
    precedence(f, rows[1]);
    body(f, rows[3]);
    status_bar(f, rows[4]);
}

fn header(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("▌", bold(AMBER)),
            Span::styled(" AIVYX PA", bold(AMBER)),
            Span::styled("  Settings", bold(FG)),
        ])),
        area,
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("~/.aivyx-pa · aivyx-pa.toml ", dim())]).right_aligned()),
        area,
    );
}

fn precedence(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" resolved ", dim()),
            Span::styled("env", Style::default().fg(LAV)),
            Span::styled(" › ", Style::default().fg(DIMMER)),
            Span::styled("aivyx-pa.toml", Style::default().fg(LAV)),
            Span::styled(" › ", Style::default().fg(DIMMER)),
            Span::styled("defaults", Style::default().fg(DIM)),
        ])),
        area,
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("[ edit aivyx-pa.toml ] ", bold(AMBER))]).right_aligned()),
        area,
    );
}

fn body(f: &mut Frame, area: Rect) {
    let cols = Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)])
        .spacing(1)
        .split(area);
    categories(f, cols[0]);
    settings(f, cols[1]);
}

fn categories(f: &mut Frame, area: Rect) {
    let cats = [
        "Model & Provider",
        "Daemon & Runtime",
        "Storage & Sandbox",
        "Memory & Recall",
        "Self-Learning",
        "Autonomous Loop",
        "Notifications",
        "Security & Audit",
    ];
    let lines: Vec<Line> = cats
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let sel = i == SEL_CAT;
            let marker = if sel { Span::styled("▌ ", bold(AMBER)) } else { Span::styled("  ", dim()) };
            let s = if sel { bold(AMBER) } else { Style::default().fg(FG) };
            Line::from(vec![marker, Span::styled(*c, s)])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(panel("CATEGORIES", true)), area);
}

fn settings(f: &mut Frame, area: Rect) {
    let sel = |label: &'static str| {
        vec![
            Span::styled("❮ ", Style::default().fg(DIMMER)),
            Span::styled(label, bold(AMBER)),
            Span::styled(" ❯", Style::default().fg(DIMMER)),
        ]
    };
    let text = |v: &'static str| vec![Span::styled(v, Style::default().fg(FG))];

    let lines = vec![
        row("provider", sel("ollama"), "env"),
        row("model", text("llama3.1"), "env"),
        row("base url", text("http://localhost:11434"), "env"),
        row("max tokens", text("4096"), "default"),
        row("api key", vec![Span::styled("— not needed for ollama", Style::default().fg(DIM))], "—"),
        Line::from(""),
        row("context", text("inferred from model"), "auto"),
        row("prompt", text("family-detected"), "auto"),
        Line::from(""),
        Line::from(Span::styled(
            "ollama is local: no API key, no egress, no per-run cost.",
            Style::default().fg(DIMMER),
        )),
        Line::from(Span::styled(
            "switch the provider above to ollama / jan / llamacpp.",
            Style::default().fg(DIMMER),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).block(panel("MODEL & PROVIDER", false)),
        area,
    );
}

fn status_bar(f: &mut Frame, area: Rect) {
    let sep = || Span::styled(" · ", Style::default().fg(DIMMER));
    let left = Line::from(vec![
        Span::styled(" Model & Provider", bold(AMBER)),
        Span::styled(" · ", dim()),
        Span::styled("provider ollama", Style::default().fg(LAV)),
        Span::styled(" · ", dim()),
        Span::styled("saved", Style::default().fg(OK)),
    ]);
    let right = Line::from(vec![
        Span::styled("↑↓ category", dim()),
        sep(),
        Span::styled("Tab field", dim()),
        sep(),
        Span::styled("Space change", dim()),
        sep(),
        Span::styled("^S save", Style::default().fg(OK)),
        sep(),
        Span::styled("^Q quit ", dim()),
    ])
    .right_aligned();
    f.render_widget(Paragraph::new(left).style(Style::default().bg(STATUS_BG)), area);
    f.render_widget(Paragraph::new(right).style(Style::default().bg(STATUS_BG)), area);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let snapshot = std::env::args().any(|a| a == "--snapshot") || !io::stdout().is_terminal();

    if snapshot {
        let mut term = Terminal::new(TestBackend::new(W, H))?;
        term.draw(draw)?;
        let buf = term.backend().buffer().clone();
        for y in 0..H {
            let mut s = String::new();
            for x in 0..W {
                s.push_str(buf[(x, y)].symbol());
            }
            println!("{}", s.trim_end());
        }
        return Ok(());
    }

    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;
    term.draw(draw)?;
    loop {
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(_) = event::read()? {
                break;
            }
        }
    }
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}
