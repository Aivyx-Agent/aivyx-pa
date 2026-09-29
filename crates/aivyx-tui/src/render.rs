//! The ratatui render layer — Phase 185 Task 3.
//!
//! A pure function of [`AppState`] → terminal frame: a scrollable
//! chat pane on top, a one-line status bar, and a bordered input line
//! (which becomes an approve/reject prompt while an approval gate is
//! pending). The visual result is **operator-verified** (no terminal
//! in CI); the headless-testable pieces are the buffer smoke test
//! (via `TestBackend`) and the pure [`chat_scroll_offset`] math.
//!
//! Long chat lines are truncated at the right edge rather than
//! wrapped, which keeps the scroll arithmetic exact (one [`ChatLine`]
//! == one row). Wrapping + precise wrapped-scroll is a refinement for
//! a later Chapter I phase, alongside live token streaming.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph};
use ratatui::Frame;

use crate::model::{AppState, LineKind, MissionPhase, StepState, View};
use crate::palette::{self, bold, fg};

/// Compute the first visible chat row given the total line count, the
/// chat viewport height, and the scroll offset (`0` == pinned to the
/// bottom, larger == scrolled up toward older lines). Pure so the
/// scroll behaviour is unit-tested without a terminal.
pub fn chat_scroll_offset(total: usize, viewport: usize, scroll: usize) -> u16 {
    // The top row when pinned to the bottom: everything that doesn't
    // fit is above the viewport.
    let max_top = total.saturating_sub(viewport);
    // Scrolling up moves the top earlier, but never before line 0.
    let top = max_top.saturating_sub(scroll);
    top.min(u16::MAX as usize) as u16
}

/// The Audit view's own version of [`chat_scroll_offset`] — same
/// signature and same key bindings (`Msg::ScrollUp`/`ScrollDown`), but
/// anchored at the opposite end: the Audit panel renders its page
/// newest-entry-first (see `render_panel`'s `.rev()`), so `0` scroll
/// pins to the *top* of the buffer (freshest visible, header included)
/// rather than chat's bottom-pinned "latest message" convention.
/// Scrolling up (`Msg::ScrollUp`, growing `scroll`) still means "go see
/// less-recent content" in both views — here that means moving the
/// viewport further down the page, toward its older tail.
pub fn audit_scroll_offset(total: usize, viewport: usize, scroll: usize) -> u16 {
    let max_top = total.saturating_sub(viewport);
    scroll.min(max_top).min(u16::MAX as usize) as u16
}

/// The display prefix + base style for a line kind. The prefix keeps
/// provenance legible even where colour is unavailable (and is what
/// the headless smoke test can assert on).
fn kind_style(kind: LineKind) -> (&'static str, Style) {
    match kind {
        LineKind::Operator => ("❯ ", bold(palette::AMBER)),
        LineKind::Agent => ("", fg(palette::FG)),
        LineKind::Tool => ("  ", fg(palette::DIM)),
        LineKind::Status => (
            "  ",
            fg(palette::DIM).add_modifier(Modifier::ITALIC),
        ),
        LineKind::Gate => ("⚑ ", bold(palette::AMBER)),
        // Routing visibility B3 — same highlighted treatment as Gate
        // (both flag something needing the operator's attention), with
        // its own glyph so a consent notice is never mistaken for an
        // actual approval gate.
        LineKind::Consent => ("☁ ", bold(palette::AMBER)),
        LineKind::System => ("· ", fg(palette::LAV)),
    }
}

/// Draw the whole UI for the current state.
pub fn render(frame: &mut Frame, state: &AppState) {
    // The near-black Aivyx canvas behind every pane.
    frame.render_widget(
        Block::new().style(Style::default().bg(palette::BG)),
        frame.area(),
    );

    // Tab bar on top of every view; the body below it.
    let outer = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(frame.area());
    render_tab_bar(frame, outer[0], state);
    let body = outer[1];

    match state.view {
        View::Chat => {
            let rows = Layout::vertical([
                Constraint::Min(1),    // chat pane
                Constraint::Length(1), // status bar
                Constraint::Length(3), // bordered input
            ])
            .split(body);
            render_chat(frame, rows[0], state);
            render_status(frame, rows[1], state);
            render_input(frame, rows[2], state);
        }
        View::Missions => {
            // A compose row appears while the operator is typing / starting a
            // new mission (Chapter L).
            let composing = state.mission_compose.is_some() || state.mission_starting;
            if composing {
                let rows = Layout::vertical([
                    Constraint::Min(1),    // master/detail body
                    Constraint::Length(3), // new-mission compose box
                    Constraint::Length(1), // status bar
                ])
                .split(body);
                render_missions(frame, rows[0], state);
                render_mission_compose(frame, rows[1], state);
                render_status(frame, rows[2], state);
            } else {
                let rows = Layout::vertical([
                    Constraint::Min(1),    // master/detail body
                    Constraint::Length(1), // status bar
                ])
                .split(body);
                render_missions(frame, rows[0], state);
                render_status(frame, rows[1], state);
            }
        }
        View::Dashboard | View::Audit | View::Tools => {
            let rows = Layout::vertical([
                Constraint::Min(1),    // panel
                Constraint::Length(1), // status bar
            ])
            .split(body);
            render_panel(frame, rows[0], state);
            render_status(frame, rows[1], state);
        }
    }
}

/// The view selector: `▌ AIVYX PA  1 Chat · 2 Dashboard · …` with the
/// active view amber, on the dark status fill.
fn render_tab_bar(frame: &mut Frame, area: Rect, state: &AppState) {
    let mut spans = vec![
        Span::styled("▌", bold(palette::AMBER)),
        Span::styled(" AIVYX PA  ", bold(palette::AMBER)),
    ];
    for (i, v) in View::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", fg(palette::DIMMER)));
        }
        let label = format!("{} {}", i + 1, v.label());
        spans.push(if *v == state.view {
            Span::styled(label, bold(palette::AMBER))
        } else {
            Span::styled(label, fg(palette::DIM))
        });
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(palette::STATUS_BG)),
        area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("Tab ⇄ views ", fg(palette::DIMMER))]))
            .right_aligned()
            .style(Style::default().bg(palette::STATUS_BG)),
        area,
    );
}

/// A bordered panel with an amber title (the read-only views' frame).
fn panel_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(fg(palette::BORDER))
        .title(Span::styled(format!(" {title} "), bold(palette::AMBER)))
        .padding(Padding::new(1, 1, 0, 0))
        .style(Style::default().bg(palette::BG))
}

/// Render the active read-only panel (Dashboard / Audit / Tools). These
/// show their frame + the state the model already holds. Audit is wired
/// to live daemon data (`/classic` retirement, Task 5) and scrolls via
/// [`audit_scroll_offset`] (the Audit-page counterpart of
/// [`chat_scroll_offset`], which `render_chat` uses for the same
/// purpose); Tools' capability/call-stats detail shipped in
/// POLISH_WAVES.md sub-project 8. Dashboard's loop/reminders/missions/
/// audit summaries (`dashboard_lines`) shipped in Phase 186 — the last
/// of this comment's own original follow-on list.
fn render_panel(frame: &mut Frame, area: Rect, state: &AppState) {
    let (title, lines) = match state.view {
        View::Dashboard => ("DASHBOARD", dashboard_lines(state)),
        View::Audit => {
            let mut lines: Vec<Line> = vec![Line::from(Span::styled(
                format!("{} total events — ← / → to page", state.audit_total),
                fg(palette::DIMMER),
            ))];
            if state.audit_entries.is_empty() {
                lines.push(Line::from(Span::styled("No entries loaded.", fg(palette::DIM))));
            } else {
                for e in state.audit_entries.iter().rev() {
                    lines.push(Line::from(vec![
                        Span::styled(format!("#{} ", e.seq), fg(palette::DIMMER)),
                        Span::styled(e.event_type.clone(), fg(palette::AMBER)),
                    ]));
                }
            }
            ("AUDIT", lines)
        }
        View::Tools => {
            let mut lines: Vec<Line> = vec![Line::from(Span::styled(
                format!("{} tool(s) — whole audit chain", state.tool_stats.len()),
                fg(palette::DIMMER),
            ))];
            if state.tool_stats.is_empty() {
                lines.push(Line::from(Span::styled("No tools loaded.", fg(palette::DIM))));
            } else {
                for t in state.tool_stats.iter() {
                    let avg_ms = t.total_duration_ms.checked_div(t.calls).unwrap_or(0);
                    let marker = if t.registered { "" } else { " [unregistered]" };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{}{marker} ", t.name), fg(palette::AMBER)),
                        Span::styled(format!("calls={} avg={avg_ms}ms", t.calls), fg(palette::DIMMER)),
                    ]));
                    if t.calls > 0 {
                        let parts: Vec<String> = t
                            .outcomes
                            .iter()
                            .map(|(label, count)| format!("{label}={count}"))
                            .collect();
                        lines.push(Line::from(Span::styled(
                            format!("  {}", parts.join(" ")),
                            fg(palette::DIM),
                        )));
                    }
                }
            }
            ("TOOLS", lines)
        }
        View::Chat | View::Missions => return,
    };

    let line_count = lines.len();
    let mut para = Paragraph::new(lines).block(panel_block(title));
    if state.view == View::Audit || state.view == View::Tools || state.view == View::Dashboard {
        // `panel_block` draws a top+bottom border (and no vertical
        // padding), so the visible text viewport is 2 rows shorter than
        // `area` — the same `chat_scroll_offset` math `render_chat` uses
        // for its own unbordered Paragraph, adjusted for that border.
        let viewport = area.height.saturating_sub(2) as usize;
        let top = audit_scroll_offset(line_count, viewport, state.scroll);
        para = para.scroll((top, 0));
    }
    frame.render_widget(para, area);
}

/// The Nonagon Missions/Fleet panel (Chapter J.7): a mission stream on the
/// left, the selected mission's step timeline on the right — the live render
/// of a team's mission DAG, fed by `Msg::MissionsUpdated`.
fn render_missions(frame: &mut Frame, area: Rect, state: &AppState) {
    let cols = Layout::horizontal([Constraint::Percentage(56), Constraint::Percentage(44)])
        .spacing(1)
        .split(area);
    render_mission_stream(frame, cols[0], state);
    render_mission_detail(frame, cols[1], state);
}

/// Phase → (badge text, style).
fn phase_badge(phase: MissionPhase) -> Span<'static> {
    let (text, color) = match phase {
        MissionPhase::Executing => ("● executing", palette::OK),
        MissionPhase::AwaitingApproval => ("⚑ approval", palette::AMBER),
        MissionPhase::Paused => ("⏸ paused", palette::AMBER),
        MissionPhase::Planning => ("◦ planning", palette::DIM),
        MissionPhase::Done => ("✓ done", palette::DIMMER),
        MissionPhase::Rejected => ("✗ rejected", palette::ERR),
        MissionPhase::Halted => ("⊘ halted", palette::ERR),
    };
    Span::styled(text, bold(color))
}

/// A `[████░░░]` progress bar `width` cells wide.
fn progress_bar(pct: u16, width: usize) -> Vec<Span<'static>> {
    let filled = (pct as usize * width / 100).min(width);
    vec![
        Span::styled("█".repeat(filled), fg(palette::AMBER)),
        Span::styled("░".repeat(width - filled), fg(palette::DIMMER)),
    ]
}

fn render_mission_stream(frame: &mut Frame, area: Rect, state: &AppState) {
    let rows = &state.missions.rows;
    if rows.is_empty() {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled("— no missions running —", fg(palette::DIM))),
            Line::from(Span::styled(
                "Run one with `aivyx-pa team run \"<mission>\"`; the lead's DAG",
                fg(palette::DIMMER),
            )),
            Line::from(Span::styled(
                "and each specialist's progress stream in here live.",
                fg(palette::DIMMER),
            )),
        ];
        frame.render_widget(Paragraph::new(lines).block(panel_block("MISSIONS")), area);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    for (i, m) in rows.iter().enumerate() {
        let sel = i == state.missions.selected;
        let marker = if sel {
            Span::styled("▌ ", bold(palette::AMBER))
        } else {
            Span::styled("  ", fg(palette::DIM))
        };
        let title_style = if sel { bold(palette::AMBER) } else { bold(palette::FG) };
        lines.push(Line::from(vec![
            marker,
            phase_badge(m.phase),
            Span::styled(format!("  {}  ", m.id), fg(palette::DIMMER)),
            Span::styled(m.title.clone(), title_style),
        ]));
        // Progress + lead on the meta line.
        let mut meta = vec![Span::styled("    ", fg(palette::DIM))];
        meta.extend(progress_bar(m.progress, 18));
        meta.push(Span::styled(format!("  {}%  · ", m.progress), fg(palette::DIM)));
        meta.push(Span::styled(m.lead.clone(), fg(palette::LAV)));
        lines.push(Line::from(meta));
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines).block(panel_block("MISSIONS")), area);
}

fn render_mission_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let Some(m) = state.missions.selected_row() else {
        frame.render_widget(
            Paragraph::new(Vec::<Line>::new()).block(panel_block("STEPS")),
            area,
        );
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    for step in &m.steps {
        let color = match step.state {
            StepState::Done => palette::OK,
            StepState::Running => palette::AMBER,
            StepState::Gated => palette::LAV,
            StepState::Failed => palette::ERR,
            StepState::Pending => palette::DIMMER,
        };
        lines.push(Line::from(vec![
            Span::styled(step.state.dot(), fg(color)),
            Span::styled("  ", fg(palette::DIM)),
            Span::styled(step.label.clone(), fg(palette::FG)),
        ]));
    }
    if m.steps.is_empty() {
        lines.push(Line::from(Span::styled("— no steps yet —", fg(palette::DIM))));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("lead · ", fg(palette::LAV)),
        Span::styled(m.lead.clone(), fg(palette::FG)),
        Span::styled(format!("   {}", m.phase.label()), fg(palette::DIMMER)),
    ]));
    // Chapter L.6 — when the mission is paused at a human gate, surface the
    // approve/reject affordance the Missions-panel keys drive.
    if m.phase == MissionPhase::AwaitingApproval {
        if let Some(gate) = &m.pending_gate {
            lines.push(Line::from(Span::styled(
                format!("⚑ gate `{gate}` awaiting your decision"),
                fg(palette::AMBER),
            )));
            lines.push(Line::from(vec![
                Span::styled("a/y", fg(palette::OK)),
                Span::styled(" approve   ", fg(palette::DIM)),
                Span::styled("r", fg(palette::ERR)),
                Span::styled(" reject", fg(palette::DIM)),
            ]));
        }
    }

    let title = format!("{} · STEPS", m.id);
    frame.render_widget(Paragraph::new(lines).block(panel_block(&title)), area);
}

fn kv<'a>(k: &'a str, v: Span<'a>) -> Line<'a> {
    Line::from(vec![Span::styled(format!("{k:<10}"), fg(palette::DIM)), v])
}

/// Phase 186 — a reminder's due time as a short relative offset. Plain
/// integer-second arithmetic (no date/time crate, matching this crate's
/// existing style — see the Global Constraints in this phase's plan).
fn format_due_offset(due_unix: i64, now_unix: i64) -> String {
    let delta = due_unix.saturating_sub(now_unix);
    let abs = delta.unsigned_abs();
    let (value, unit) = if abs < 60 {
        (abs, "s")
    } else if abs < 3_600 {
        (abs / 60, "m")
    } else if abs < 86_400 {
        (abs / 3_600, "h")
    } else {
        (abs / 86_400, "d")
    };
    if delta >= 0 {
        format!("in {value}{unit}")
    } else {
        format!("{value}{unit} overdue")
    }
}

fn dashboard_lines(state: &AppState) -> Vec<Line<'_>> {
    let role = state.status.role.as_deref().unwrap_or("—");
    let daemon = if state.status.daemon_connected {
        Span::styled("connected ✓", fg(palette::OK))
    } else {
        Span::styled("offline ✗", fg(palette::ERR))
    };
    let status = if state.status.working {
        Span::styled("working…", fg(palette::LAV))
    } else {
        Span::styled("idle", fg(palette::FG))
    };

    let mut lines = vec![
        kv("role", Span::styled(role.to_string(), fg(palette::FG))),
        kv("daemon", daemon),
        kv("status", status),
        kv(
            "session",
            Span::styled(format!("{} lines", state.history.len()), fg(palette::FG)),
        ),
        Line::from(""),
    ];

    // --- Loop ---
    lines.push(Line::from(Span::styled("LOOP", bold(palette::AMBER))));
    match &state.loop_status {
        None => lines.push(Line::from(Span::styled("idle — not yet fetched", fg(palette::DIM)))),
        Some(ls) => {
            if ls.state.consecutive_idle > 0 && ls.state.active {
                lines.push(Line::from(Span::styled(
                    format!("stalled ({} consecutive idle)", ls.state.consecutive_idle),
                    fg(palette::ERR),
                )));
            } else if ls.state.active {
                lines.push(Line::from(Span::styled(
                    format!(
                        "running (iter {}/{}, ${:.2}, {}k tokens)",
                        ls.state.iteration,
                        ls.state.max_iterations,
                        ls.state.spent_cents as f64 / 100.0,
                        ls.state.tokens_used / 1_000,
                    ),
                    fg(palette::LAV),
                )));
            } else {
                let reason = ls.state.last_stop_reason.as_deref().unwrap_or("never run");
                lines.push(Line::from(Span::styled(format!("idle ({reason})"), fg(palette::FG))));
            }
        }
    }
    lines.push(Line::from(""));

    // --- Reminders ---
    lines.push(Line::from(Span::styled("REMINDERS", bold(palette::AMBER))));
    match &state.reminders {
        None => lines.push(Line::from(Span::styled("not yet fetched", fg(palette::DIM)))),
        Some(reminders) if reminders.is_empty() => {
            lines.push(Line::from(Span::styled("none pending", fg(palette::DIM))));
        }
        Some(reminders) => {
            lines.push(Line::from(Span::styled(
                format!("{} pending", reminders.len()),
                fg(palette::FG),
            )));
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            // Soonest-due-first. `ReminderStore::list` already returns
            // entries in this order (sorted by `(due_unix, id)`), but
            // sort defensively here so the panel's display order doesn't
            // silently depend on the caller's fetch order — using the
            // same full tiebreaker so ties resolve identically to the
            // store's own order.
            let mut sorted: Vec<&aivyx_channel::daemon_ipc::ReminderView> = reminders.iter().collect();
            sorted.sort_by_key(|r| (r.due_unix, r.id.clone()));
            for r in sorted.into_iter().take(3) {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<12}", format_due_offset(r.due_unix, now_unix)), fg(palette::DIMMER)),
                    Span::styled(r.message.clone(), fg(palette::FG)),
                ]));
            }
        }
    }
    lines.push(Line::from(""));

    // --- Missions ---
    lines.push(Line::from(Span::styled("MISSIONS", bold(palette::AMBER))));
    if state.missions.rows.is_empty() {
        lines.push(Line::from(Span::styled("none", fg(palette::DIM))));
    } else {
        let active = state
            .missions
            .rows
            .iter()
            .filter(|m| !matches!(m.phase, crate::model::MissionPhase::Done))
            .count();
        let done = state.missions.rows.len() - active;
        lines.push(Line::from(Span::styled(
            format!("{active} active, {done} done"),
            fg(palette::FG),
        )));
    }
    lines.push(Line::from(""));

    // --- Audit ---
    lines.push(Line::from(Span::styled("AUDIT", bold(palette::AMBER))));
    lines.push(Line::from(Span::styled(
        format!("{} total events", state.audit_total),
        fg(palette::FG),
    )));
    for e in state.audit_entries.iter().rev().take(3) {
        lines.push(Line::from(vec![
            Span::styled(format!("#{} ", e.seq), fg(palette::DIMMER)),
            Span::styled(e.event_type.clone(), fg(palette::AMBER)),
        ]));
    }

    lines
}

fn render_chat(frame: &mut Frame, area: Rect, state: &AppState) {
    // Wrap each history line to the pane width (coalesced agent
    // replies are full paragraphs now — Vitrine §12); continuation
    // rows indent under the prefix so provenance stays scannable.
    // Wrapping here (not via Paragraph::wrap) keeps the scroll
    // offset math operating on real visual-line counts.
    let width = area.width.max(1) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for cl in &state.history {
        let (prefix, style) = kind_style(cl.kind);
        let avail = width.saturating_sub(prefix.chars().count()).max(1);
        for (i, chunk) in wrap_line(&cl.text, avail).into_iter().enumerate() {
            let lead = if i == 0 {
                prefix.to_string()
            } else {
                " ".repeat(prefix.chars().count())
            };
            lines.push(Line::from(vec![
                Span::styled(lead, style),
                Span::styled(chunk, style),
            ]));
        }
    }

    let top = chat_scroll_offset(lines.len(), area.height as usize, state.scroll);
    let para = Paragraph::new(Text::from(lines)).scroll((top, 0));
    frame.render_widget(para, area);
}

/// Greedy width-wrap, breaking at the last space when one exists in
/// the overflowing line (words longer than the width hard-break).
/// Empty text yields one empty chunk so intended blank lines survive.
fn wrap_line(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut count = 0usize;
    for ch in text.chars() {
        line.push(ch);
        count += 1;
        if count >= width {
            if let Some(pos) = line.rfind(' ') {
                let rest = line.split_off(pos + 1);
                out.push(std::mem::take(&mut line));
                line = rest;
            } else {
                out.push(std::mem::take(&mut line));
            }
            count = line.chars().count();
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

fn render_status(frame: &mut Frame, area: Rect, state: &AppState) {
    let sep = || Span::styled(" · ", fg(palette::DIM));

    // Left: role · daemon · (gate | working).
    let role = state.status.role.as_deref().unwrap_or("—");
    let mut left = vec![
        Span::styled(format!(" {role}"), bold(palette::AMBER)),
        sep(),
        if state.status.daemon_connected {
            Span::styled("daemon ✓", fg(palette::OK))
        } else {
            Span::styled("daemon ✗", fg(palette::ERR))
        },
    ];
    // Routing visibility B3 — the conversation's routed model, when one
    // has been chosen. Sits between the daemon indicator and the
    // gate/working indicator, which is a rarer, more urgent state and
    // so stays last (closest to the visual "attention" edge).
    if let Some(model) = state.status.routed_model.as_deref() {
        left.push(sep());
        left.push(Span::styled(format!("model {model}"), fg(palette::LAV)));
    }
    if state.gate.is_some() {
        left.push(sep());
        left.push(Span::styled("⚑ approval needed", bold(palette::AMBER)));
    } else if state.status.working {
        left.push(sep());
        left.push(Span::styled("working…", fg(palette::LAV)));
    }

    // Right: context-appropriate keybinding help.
    let help: Vec<Span> = if state.gate.is_some() {
        vec![
            Span::styled("y approve", fg(palette::OK)),
            sep(),
            Span::styled("n reject", fg(palette::FG)),
            sep(),
            Span::styled("^Q quit ", fg(palette::DIM)),
        ]
    } else if state.view == View::Missions && state.mission_compose.is_some() {
        vec![Span::styled(
            "type a goal · Enter start · Esc cancel ",
            fg(palette::DIM),
        )]
    } else if state.view == View::Missions {
        vec![Span::styled(
            "n new · ↑↓ select · a approve · r reject · Esc chat · ^Q quit ",
            fg(palette::DIM),
        )]
    } else if state.view != View::Chat {
        vec![Span::styled(
            "Tab views · 1-5 jump · ↑↓ scroll · Esc chat · ^Q quit ",
            fg(palette::DIM),
        )]
    } else {
        vec![Span::styled(
            "Tab views · ^Q quit · PgUp/PgDn scroll · Esc cancel ",
            fg(palette::DIM),
        )]
    };

    // Left status, right-aligned help on the same row.
    let span_w = |spans: &[Span]| spans.iter().map(|s| s.content.chars().count()).sum::<usize>();
    let pad = (area.width as usize).saturating_sub(span_w(&left) + span_w(&help));

    let mut spans = left;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(help);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(palette::STATUS_BG)),
        area,
    );
}

fn render_input(frame: &mut Frame, area: Rect, state: &AppState) {
    if let Some(gate) = &state.gate {
        let prompt = format!("Approve? [y/n] — {}", gate.reason);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Span::styled(" Approval gate ", bold(palette::AMBER)))
            .border_style(fg(palette::AMBER))
            .style(Style::default().bg(palette::BG));
        frame.render_widget(
            Paragraph::new(prompt).style(fg(palette::AMBER)).block(block),
            area,
        );
        return;
    }

    let title = if state.status.working {
        " Input (working…) "
    } else {
        " Input "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(fg(palette::BORDER))
        .title(Span::styled(title, fg(palette::DIM)))
        .style(Style::default().bg(palette::BG));
    frame.render_widget(
        Paragraph::new(state.input.as_str())
            .style(fg(palette::FG))
            .block(block),
        area,
    );

    // Place the terminal cursor inside the bordered input at the
    // current char position. Approximate (char count, not grapheme
    // width); refined alongside wrapping in a later phase.
    let cx = area.x + 1 + state.cursor.min(area.width.saturating_sub(2) as usize) as u16;
    let cy = area.y + 1;
    frame.set_cursor_position(Position::new(cx, cy));
}

/// Chapter L — the "new mission" compose box: a bordered goal input, or a
/// "starting…" indicator while the daemon decomposes the goal.
fn render_mission_compose(frame: &mut Frame, area: Rect, state: &AppState) {
    if state.mission_starting {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(fg(palette::LAV))
            .title(Span::styled(" Starting mission… ", bold(palette::LAV)))
            .style(Style::default().bg(palette::BG));
        frame.render_widget(
            Paragraph::new("decomposing the goal into a plan…")
                .style(fg(palette::LAV))
                .block(block),
            area,
        );
        return;
    }

    let goal = state.mission_compose.as_deref().unwrap_or("");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(fg(palette::AMBER))
        .title(Span::styled(
            " New mission · Enter start · Esc cancel ",
            bold(palette::AMBER),
        ))
        .style(Style::default().bg(palette::BG));
    frame.render_widget(Paragraph::new(goal).style(fg(palette::FG)).block(block), area);

    // Cursor at the end of the typed goal.
    let len = goal.chars().count();
    let cx = area.x + 1 + (len as u16).min(area.width.saturating_sub(2));
    frame.set_cursor_position(Position::new(cx, area.y + 1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChatLine, PendingGate};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn scroll_offset_pins_to_bottom() {
        // 10 lines, 4-row viewport, pinned: top = 6.
        assert_eq!(chat_scroll_offset(10, 4, 0), 6);
        // Scrolled up by 2: top = 4.
        assert_eq!(chat_scroll_offset(10, 4, 2), 4);
        // Scrolled up past the top clamps to 0.
        assert_eq!(chat_scroll_offset(10, 4, 100), 0);
        // Everything fits: no offset.
        assert_eq!(chat_scroll_offset(3, 10, 0), 0);
    }

    #[test]
    fn audit_scroll_offset_pins_to_top() {
        // 10 lines, 4-row viewport, unscrolled: shows the top (freshest).
        assert_eq!(audit_scroll_offset(10, 4, 0), 0);
        // Scrolled down (toward older entries) by 2: top = 2.
        assert_eq!(audit_scroll_offset(10, 4, 2), 2);
        // Scrolled past the bottom clamps to max_top, not past it.
        assert_eq!(audit_scroll_offset(10, 4, 100), 6);
        // Everything fits: no offset regardless of scroll.
        assert_eq!(audit_scroll_offset(3, 10, 5), 0);
    }

    #[test]
    fn renders_chat_and_status_into_buffer() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut state = AppState::new();
        state.status.daemon_connected = true;
        state.status.role = Some("assistant".into());
        state.history.push(ChatLine {
            kind: LineKind::Operator,
            text: "hello there".into(),
        });
        state.history.push(ChatLine {
            kind: LineKind::Agent,
            text: "general kenobi".into(),
        });

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("hello there"), "operator line rendered");
        assert!(text.contains("general kenobi"), "agent line rendered");
        assert!(text.contains("assistant"), "role in status bar");
        assert!(text.contains("daemon ✓"), "daemon status rendered");
        assert!(text.contains("Input"), "input block titled");
        // The tab bar is present on every view.
        assert!(text.contains("Chat"), "tab bar lists Chat");
        assert!(text.contains("Dashboard"), "tab bar lists Dashboard");
    }

    // ---- Routing visibility B3 ----

    #[test]
    fn status_bar_shows_the_routed_model() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut state = AppState::new();
        state.status.daemon_connected = true;
        state.status.role = Some("assistant".into());
        state.status.routed_model = Some("small@default".into());

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("model small@default"), "got: {text}");
    }

    #[test]
    fn status_bar_shows_no_model_segment_before_any_routed_decision() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        let state = AppState::new();
        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(!text.contains("model "), "got: {text}");
    }

    #[test]
    fn cloud_consent_notice_is_styled_like_gate() {
        assert_eq!(
            kind_style(LineKind::Consent).1,
            kind_style(LineKind::Gate).1,
            "a consent notice must get the same highlighted treatment as \
             an approval gate"
        );
    }

    #[test]
    fn cloud_consent_notice_renders_in_the_chat_pane() {
        let backend = TestBackend::new(72, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut state = AppState::new();
        state.history.push(ChatLine {
            kind: LineKind::Consent,
            text: "cloud consent needed — `claude-sonnet-4-5` (your `anthropic` \
                   endpoint)"
                .into(),
        });

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("cloud consent needed"), "got: {text}");
        assert!(text.contains("☁"), "got: {text}");
    }

    #[test]
    fn dashboard_view_renders_panel_not_chat_input() {
        let backend = TestBackend::new(72, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.status.daemon_connected = true;
        state.status.role = Some("researcher".into());

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        // The panel renders (not the chat input line).
        assert!(text.contains("DASHBOARD"), "panel titled");
        assert!(text.contains("researcher"), "role shown in panel");
        assert!(!text.contains(" Input "), "no chat input in a panel view");
        // Tab bar still present.
        assert!(text.contains("Audit"), "tab bar lists Audit");
    }

    #[test]
    fn dashboard_shows_idle_loop_and_no_reminders_by_default() {
        let backend = TestBackend::new(72, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Dashboard;

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("idle"), "no loop_status fetched yet reads as idle/unknown");
        assert!(
            text.contains("not yet fetched"),
            "no reminders fetched yet reads distinctly from a genuinely empty list"
        );
    }

    #[test]
    fn dashboard_shows_none_pending_once_fetched_empty() {
        // Distinct from `dashboard_shows_idle_loop_and_no_reminders_by_default`
        // above: `Some(vec![])` (a successful fetch that found nothing) must
        // render differently from `None` (never fetched / fetch errored) —
        // this is the Important final-review finding this diff fixes.
        let backend = TestBackend::new(72, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.reminders = Some(vec![]);

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("none pending"), "fetched successfully, genuinely empty");
    }

    #[test]
    fn dashboard_shows_running_loop_status() {
        let backend = TestBackend::new(72, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.loop_status = Some(crate::model::LoopStatusView {
            state: aivyx_channel::loop_driver::LoopRunState {
                active: true,
                iteration: 3,
                max_iterations: 10,
                spent_cents: 250,
                tokens_used: 4_000,
                ..Default::default()
            },
            remaining: 2,
            armed: true,
            gate_enabled: false,
            max_run_secs: None,
            max_run_tokens: None,
            max_run_usd: None,
            max_idle_iterations: 0,
        });

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("3/10"), "iteration/max shown");
        assert!(text.contains("running"), "active loop reads as running");
    }

    #[test]
    fn loop_status_formats_integer_division_edge_cases_exactly() {
        // Finding 6 — direct exact-string check on the LOOP formatter's
        // integer-division edges: `tokens_used / 1_000` truncates (not
        // rounds), and `spent_cents as f64 / 100.0` formats as `$X.XX`.
        let backend = TestBackend::new(72, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.loop_status = Some(crate::model::LoopStatusView {
            state: aivyx_channel::loop_driver::LoopRunState {
                active: true,
                iteration: 1,
                max_iterations: 1,
                spent_cents: 250,
                tokens_used: 999, // just under 1_000 -> truncates to 0k
                ..Default::default()
            },
            remaining: 0,
            armed: true,
            gate_enabled: false,
            max_run_secs: None,
            max_run_tokens: None,
            max_run_usd: None,
            max_idle_iterations: 0,
        });

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("$2.50"), "250 cents formats as $2.50");
        assert!(text.contains("0k tokens"), "999 tokens truncates to 0k, not 1k");

        // A second fixture: 4_999 tokens truncates to 4k, not 5k.
        let backend2 = TestBackend::new(72, 20);
        let mut terminal2 = Terminal::new(backend2).unwrap();
        let mut state2 = AppState::new();
        state2.view = View::Dashboard;
        state2.loop_status = Some(crate::model::LoopStatusView {
            state: aivyx_channel::loop_driver::LoopRunState {
                active: true,
                iteration: 1,
                max_iterations: 1,
                spent_cents: 250,
                tokens_used: 4_999,
                ..Default::default()
            },
            remaining: 0,
            armed: true,
            gate_enabled: false,
            max_run_secs: None,
            max_run_tokens: None,
            max_run_usd: None,
            max_idle_iterations: 0,
        });

        terminal2.draw(|f| render(f, &state2)).unwrap();
        let text2 = buffer_text(&terminal2);

        assert!(text2.contains("4k tokens"), "4_999 tokens truncates to 4k, not 5k");
    }

    #[test]
    fn format_due_offset_exact_strings() {
        // Finding 6 — direct exact-output checks, one per bucket plus the
        // zero-delta and overdue edges.
        assert_eq!(format_due_offset(1_000, 1_000), "in 0s", "delta exactly 0");
        assert_eq!(format_due_offset(1_030, 1_000), "in 30s", "seconds bucket");
        assert_eq!(format_due_offset(1_000 + 5 * 60, 1_000), "in 5m", "minutes bucket");
        assert_eq!(format_due_offset(1_000 + 3 * 3_600, 1_000), "in 3h", "hours bucket");
        assert_eq!(format_due_offset(1_000 + 2 * 86_400, 1_000), "in 2d", "days bucket");
        assert_eq!(format_due_offset(940, 1_000), "1m overdue", "past/overdue value");
    }

    #[test]
    fn format_due_offset_saturates_instead_of_overflowing() {
        // Finding 5 — an out-of-range `due_unix` must not panic.
        let text = format_due_offset(i64::MIN, i64::MAX);
        assert!(text.ends_with("overdue"), "still produces a sane overdue string: {text}");
        let text = format_due_offset(i64::MAX, i64::MIN);
        assert!(text.starts_with("in "), "still produces a sane future string: {text}");
    }

    #[test]
    fn dashboard_shows_stalled_loop_status() {
        let backend = TestBackend::new(72, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.loop_status = Some(crate::model::LoopStatusView {
            state: aivyx_channel::loop_driver::LoopRunState {
                active: true,
                consecutive_idle: 4,
                ..Default::default()
            },
            remaining: 0,
            armed: true,
            gate_enabled: false,
            max_run_secs: None,
            max_run_tokens: None,
            max_run_usd: None,
            max_idle_iterations: 5,
        });

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("stalled"), "consecutive_idle > 0 reads as stalled");
    }

    #[test]
    fn dashboard_shows_next_reminders_soonest_first() {
        let backend = TestBackend::new(72, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.reminders = Some(vec![
            aivyx_channel::daemon_ipc::ReminderView {
                id: "r1".into(),
                due_unix: 300,
                message: "call mom".into(),
                notify_targets: vec![],
                created_unix: 0,
            },
            aivyx_channel::daemon_ipc::ReminderView {
                id: "r2".into(),
                due_unix: 100,
                message: "standup".into(),
                notify_targets: vec![],
                created_unix: 0,
            },
        ]);

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("2 pending"), "count shown");
        assert!(text.contains("standup"), "soonest reminder shown");
        let standup_pos = text.find("standup").unwrap();
        let call_mom_pos = text.find("call mom").unwrap();
        assert!(standup_pos < call_mom_pos, "soonest (standup, due 100) listed before due 300");
    }

    #[test]
    fn dashboard_summarizes_missions_by_phase() {
        use crate::model::{MissionPhase, MissionRow};
        let backend = TestBackend::new(72, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.missions.rows = vec![
            MissionRow {
                id: "m1".into(),
                title: "t1".into(),
                lead: "aria".into(),
                phase: MissionPhase::Executing,
                progress: 40,
                steps: vec![],
                pending_gate: None,
            },
            MissionRow {
                id: "m2".into(),
                title: "t2".into(),
                lead: "aria".into(),
                phase: MissionPhase::Done,
                progress: 100,
                steps: vec![],
                pending_gate: None,
            },
        ];

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("1 active"), "one Executing mission counted");
        assert!(text.contains("1 done"), "one Done mission counted");
    }

    #[test]
    fn dashboard_summarizes_audit_total_and_recent() {
        use aivyx_channel::daemon_ipc::AuditEntrySummary;
        // Taller than the other Dashboard tests: this fixture's LOOP +
        // REMINDERS + MISSIONS + AUDIT sections (with one audit entry) add
        // up to 17 content rows, one more than a 72x20 terminal's 16-row
        // panel viewport can show — bump the height so the most-recent
        // audit entry isn't clipped before the assertion below sees it.
        let backend = TestBackend::new(72, 22);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.audit_total = 42;
        state.audit_entries = vec![AuditEntrySummary {
            seq: 42,
            appended_at_unix_ms: 1_000,
            event_type: "ToolCall".into(),
            event: serde_json::json!({}),
            mac_hex: "aaa".into(),
        }];

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("42"), "audit total shown");
        assert!(text.contains("ToolCall"), "most recent event type shown");
    }

    #[test]
    fn dashboard_scroll_max_matches_dashboard_lines() {
        // Task 3 review finding — `dashboard_scroll_max` (model.rs) is a
        // hand-derived formula for `dashboard_lines`' real line count.
        // POLISH_WAVES.md sub-project 8 hit exactly this bug class in the
        // Tools view: a hand-derived clamp formula that looked right but
        // drifted from the real render output, permanently hiding the
        // bottom rows on short terminals. This test ties the two together
        // so they can never silently drift apart again.
        use aivyx_channel::daemon_ipc::{AuditEntrySummary, ReminderView};
        use crate::model::dashboard_scroll_max;

        fn reminder(n: u64) -> ReminderView {
            ReminderView {
                id: format!("r{n}"),
                due_unix: n as i64,
                message: format!("reminder {n}"),
                notify_targets: vec![],
                created_unix: 0,
            }
        }
        fn audit_entry(seq: u64) -> AuditEntrySummary {
            AuditEntrySummary {
                seq,
                appended_at_unix_ms: seq * 1_000,
                event_type: format!("Event{seq}"),
                event: serde_json::json!({}),
                mac_hex: "aaa".into(),
            }
        }

        // (a) fresh/empty AppState: no loop_status, no reminders, no
        // missions, no audit entries.
        let empty = AppState::new();
        assert_eq!(
            dashboard_scroll_max(&empty),
            dashboard_lines(&empty).len(),
            "fresh AppState"
        );

        // (b) 1 reminder + 1 audit entry — under the 3-item truncation caps.
        let mut under_cap = AppState::new();
        under_cap.reminders = Some(vec![reminder(0)]);
        under_cap.audit_entries = vec![audit_entry(0)];
        assert_eq!(
            dashboard_scroll_max(&under_cap),
            dashboard_lines(&under_cap).len(),
            "1 reminder + 1 audit entry"
        );

        // (c) 5 reminders + 5 audit entries — over the 3-item truncation
        // caps, so this actually exercises the `.min(3)` branches.
        let mut over_cap = AppState::new();
        over_cap.reminders = Some((0u64..5).map(reminder).collect());
        over_cap.audit_entries = (0u64..5).map(audit_entry).collect();
        assert_eq!(
            dashboard_scroll_max(&over_cap),
            dashboard_lines(&over_cap).len(),
            "5 reminders + 5 audit entries"
        );

        // (d) a Some(...) loop_status — previously `loop_status` stayed
        // `None` in every case above, leaving the LOOP section's `Some`
        // branch (and the None-reminders "not yet fetched" branch, which
        // every prior case above also left untouched via `AppState::new()`'s
        // default `None`) unverified by this cross-check. Traced against
        // `dashboard_lines`: the LOOP section always pushes exactly one
        // status line whether `loop_status` is `None` or `Some(..)` (the
        // `match` on `state.loop_status`'s three arms each push exactly one
        // `Line`), so the fixed "3 fixed lines" (header + 1 status + blank)
        // baked into `dashboard_scroll_max`'s `15` constant should not need
        // to change here — confirmed rather than assumed by this case.
        let mut with_loop_status = AppState::new();
        with_loop_status.loop_status = Some(crate::model::LoopStatusView {
            state: aivyx_channel::loop_driver::LoopRunState {
                active: true,
                iteration: 3,
                max_iterations: 10,
                ..Default::default()
            },
            remaining: 2,
            armed: true,
            gate_enabled: false,
            max_run_secs: None,
            max_run_tokens: None,
            max_run_usd: None,
            max_idle_iterations: 0,
        });
        assert_eq!(
            dashboard_scroll_max(&with_loop_status),
            dashboard_lines(&with_loop_status).len(),
            "Some(loop_status)"
        );

        // (e) non-empty `missions.rows` — likewise previously unverified.
        // Traced against `dashboard_lines`: the MISSIONS section always
        // pushes exactly one summary line whether `missions.rows` is empty
        // ("none") or not ("N active, M done"), so this too should leave
        // the `15` constant unchanged — confirmed here rather than assumed.
        use crate::model::{MissionPhase, MissionRow};
        let mut with_missions = AppState::new();
        with_missions.missions.rows = vec![MissionRow {
            id: "m1".into(),
            title: "t1".into(),
            lead: "aria".into(),
            phase: MissionPhase::Executing,
            progress: 40,
            steps: vec![],
            pending_gate: None,
        }];
        assert_eq!(
            dashboard_scroll_max(&with_missions),
            dashboard_lines(&with_missions).len(),
            "non-empty missions.rows"
        );
    }

    #[test]
    fn dashboard_view_scrolls_to_reveal_entries_below_the_fold() {
        // Task 3 review finding — Dashboard had no scroll mechanism at
        // all (unlike Audit/Tools), so on a short terminal its LOOP/
        // REMINDERS/MISSIONS/AUDIT content silently truncated with no
        // indicator and no way to see the rest. This proves the fix
        // works end-to-end: content clipped before scrolling becomes
        // visible after `Msg::ScrollUp`.
        use aivyx_channel::daemon_ipc::{AuditEntrySummary, ReminderView};
        let backend = TestBackend::new(40, 10); // small panel viewport
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Dashboard;
        state.status.role = Some("researcher".into());
        state.reminders = Some(
            (0u64..5)
                .map(|n| ReminderView {
                    id: format!("r{n}"),
                    due_unix: n as i64,
                    message: format!("reminder {n}"),
                    notify_targets: vec![],
                    created_unix: 0,
                })
                .collect(),
        );
        state.audit_total = 5;
        state.audit_entries = (0u64..5)
            .map(|seq| AuditEntrySummary {
                seq,
                appended_at_unix_ms: seq * 1_000,
                event_type: format!("Event{seq}"),
                event: serde_json::json!({}),
                mac_hex: "aaa".into(),
            })
            .collect();

        terminal.draw(|f| render(f, &state)).unwrap();
        let unscrolled = buffer_text(&terminal);
        assert!(unscrolled.contains("researcher"), "top-of-panel content visible by default");
        assert!(
            !unscrolled.contains("Event4"),
            "the bottom AUDIT section doesn't fit before scrolling"
        );

        state = crate::model::update(state, crate::model::Msg::ScrollUp(1_000));
        terminal.draw(|f| render(f, &state)).unwrap();
        let scrolled = buffer_text(&terminal);
        assert!(
            scrolled.contains("Event4"),
            "scrolling down must reach the bottom AUDIT section: {scrolled}"
        );
        assert_ne!(scrolled, unscrolled, "scrolling must actually change what's rendered");
    }

    #[test]
    fn missions_view_renders_stream_and_selected_steps() {
        use crate::model::{MissionPhase, MissionRow, MissionStep, StepState};
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut state = AppState::new();
        state.view = View::Missions;
        state.missions.rows = vec![
            MissionRow {
                id: "m-aria".into(),
                title: "Run end-of-day BOH close".into(),
                lead: "aria".into(),
                phase: MissionPhase::Executing,
                progress: 50,
                steps: vec![
                    MissionStep { label: "stocktake — count".into(), state: StepState::Done },
                    MissionStep { label: "inventory — low stock".into(), state: StepState::Running },
                ],
                pending_gate: None,
            },
            MissionRow {
                id: "m-2".into(),
                title: "second mission".into(),
                lead: "coordinator".into(),
                phase: MissionPhase::Planning,
                progress: 0,
                steps: vec![],
                pending_gate: None,
            },
        ];

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("MISSIONS"), "stream panel titled");
        assert!(text.contains("Run end-of-day BOH close"), "mission title rendered");
        assert!(text.contains("executing"), "phase badge rendered");
        // The selected mission's steps appear in the detail panel.
        assert!(text.contains("STEPS"), "detail panel titled");
        assert!(text.contains("stocktake — count"), "selected mission's steps shown");
        assert!(text.contains("aria"), "lead shown");
        assert!(!text.contains(" Input "), "no chat input in the Missions panel");
        // Tab bar lists the new view.
        assert!(text.contains("Missions"), "tab bar lists Missions");
    }

    #[test]
    fn awaiting_mission_renders_the_approve_reject_affordance() {
        use crate::model::{MissionPhase, MissionRow, MissionStep, StepState};
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut state = AppState::new();
        state.view = View::Missions;
        state.missions.rows = vec![MissionRow {
            id: "m-1".into(),
            title: "ship the note".into(),
            lead: "coordinator".into(),
            phase: MissionPhase::AwaitingApproval,
            progress: 33,
            steps: vec![MissionStep {
                label: "approve — reviewer (gate)".into(),
                state: StepState::Gated,
            }],
            pending_gate: Some("approve".into()),
        }];

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("gate `approve` awaiting"), "gate affordance shown");
        assert!(text.contains("approve"), "approve key hint shown");
        assert!(text.contains("reject"), "reject key hint shown");
    }

    #[test]
    fn new_mission_compose_box_renders_the_goal_and_hints() {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Missions;
        state.mission_compose = Some("close the kitchen".into());

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("New mission"), "compose box titled");
        assert!(text.contains("close the kitchen"), "typed goal shown");
        assert!(text.contains("Enter start"), "submit hint shown");
    }

    #[test]
    fn starting_mission_shows_progress_indicator() {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Missions;
        state.mission_starting = true;

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Starting mission"), "starting indicator shown");
        assert!(text.contains("decomposing"), "decomposition note shown");
    }

    #[test]
    fn missions_view_shows_empty_state() {
        let backend = TestBackend::new(90, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Missions;
        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("no missions running"), "empty-state hint shown");
        assert!(text.contains("aivyx-pa team run"), "points at the command");
    }

    #[test]
    fn wrap_line_breaks_at_spaces_and_preserves_content() {
        let chunks = wrap_line("the quick brown fox jumps over the lazy dog", 12);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.chars().count() <= 12));
        assert_eq!(chunks.concat(), "the quick brown fox jumps over the lazy dog");
        // Blank lines survive as one empty chunk.
        assert_eq!(wrap_line("", 12), vec![String::new()]);
        // Overlong single words hard-break instead of overflowing.
        let long = wrap_line("abcdefghijklmnop", 5);
        assert!(long.iter().all(|c| c.chars().count() <= 5));
        assert_eq!(long.concat(), "abcdefghijklmnop");
    }

    #[test]
    fn audit_view_shows_empty_state_before_any_fetch() {
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Audit;

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("AUDIT"), "panel titled");
        assert!(text.contains("0 total events"), "total shown even at zero");
        assert!(text.contains("No entries loaded"), "empty state shown");
        assert!(!text.contains("Phase 186"), "placeholder copy is gone");
    }

    #[test]
    fn audit_view_renders_entries_newest_first() {
        use aivyx_channel::daemon_ipc::AuditEntrySummary;
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Audit;
        state.audit_total = 2;
        state.audit_entries = vec![
            AuditEntrySummary {
                seq: 0,
                appended_at_unix_ms: 1_000,
                event_type: "TurnStarted".into(),
                event: serde_json::json!({}),
                mac_hex: "aaa".into(),
            },
            AuditEntrySummary {
                seq: 1,
                appended_at_unix_ms: 2_000,
                event_type: "TurnEnded".into(),
                event: serde_json::json!({}),
                mac_hex: "bbb".into(),
            },
        ];

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("2 total events"), "total shown");
        assert!(text.contains("#0"), "seq 0 rendered");
        assert!(text.contains("#1"), "seq 1 rendered");
        assert!(text.contains("TurnStarted"), "event type rendered");
        assert!(text.contains("TurnEnded"), "event type rendered");
        // Newest first: seq 1 ("TurnEnded") appears before seq 0
        // ("TurnStarted") in the rendered buffer.
        let pos_1 = text.find("TurnEnded").unwrap();
        let pos_0 = text.find("TurnStarted").unwrap();
        assert!(pos_1 < pos_0, "newest entry (seq 1) renders above seq 0");
    }

    #[test]
    fn tools_view_renders_placeholder_copy_is_gone() {
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Tools;

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("No tools loaded"), "empty state shown");
        assert!(!text.contains("capability scope"), "placeholder copy is gone");
    }

    #[test]
    fn tools_view_renders_call_stats() {
        use aivyx_channel::daemon_ipc::ToolStat;
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Tools;
        let mut outcomes = std::collections::BTreeMap::new();
        outcomes.insert("completed".to_string(), 3u64);
        state.tool_stats = vec![ToolStat {
            name: "fs.read".to_string(),
            description: "read a file".to_string(),
            scope_base: "fs.read".to_string(),
            registered: true,
            calls: 3,
            outcomes,
            total_duration_ms: 30,
        }];

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("fs.read"), "tool name rendered");
        assert!(text.contains("calls=3"), "call count rendered");
        assert!(text.contains("completed=3"), "outcome breakdown rendered");
    }

    #[test]
    fn tools_view_marks_unregistered_tools() {
        use aivyx_channel::daemon_ipc::ToolStat;
        let backend = TestBackend::new(72, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Tools;
        state.tool_stats = vec![ToolStat {
            name: "old.removed.tool".to_string(),
            description: "(no registered tool)".to_string(),
            scope_base: "old.removed.tool".to_string(),
            registered: false,
            calls: 1,
            outcomes: std::collections::BTreeMap::new(),
            total_duration_ms: 5,
        }];

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("[unregistered]"), "unregistered marker rendered");
    }

    #[test]
    fn audit_view_scrolls_to_reveal_entries_below_the_fold() {
        // Final-review finding 2 — a short terminal + many entries means
        // the page overflows the panel; before this fix `render_panel`
        // never called `.scroll(...)` at all, so the oldest entries were
        // permanently unreachable no matter what `state.scroll` held.
        use aivyx_channel::daemon_ipc::AuditEntrySummary;
        let backend = TestBackend::new(40, 8); // ~6 text rows inside the border
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Audit;
        state.audit_total = 20;
        state.audit_entries = (0..20u64)
            .map(|seq| AuditEntrySummary {
                seq,
                appended_at_unix_ms: seq * 1_000,
                event_type: format!("Event{seq}"),
                event: serde_json::json!({}),
                mac_hex: "aaa".into(),
            })
            .collect();

        terminal.draw(|f| render(f, &state)).unwrap();
        let unscrolled = buffer_text(&terminal);
        assert!(unscrolled.contains("Event19"), "newest entry visible by default");
        assert!(
            !unscrolled.contains("Event0 "),
            "the oldest entry doesn't fit before scrolling"
        );

        // Scroll all the way toward the older tail of the page (a scroll
        // request larger than the page clamps at the oldest entry rather
        // than panicking or no-oping).
        state = crate::model::update(state, crate::model::Msg::ScrollUp(30));
        terminal.draw(|f| render(f, &state)).unwrap();
        let scrolled = buffer_text(&terminal);
        assert!(
            scrolled.contains("Event0"),
            "scrolling to the bottom of the page must reach the oldest entry: {scrolled}"
        );
        assert_ne!(scrolled, unscrolled, "scrolling must actually change what's rendered");
    }

    #[test]
    fn tools_view_scrolls_to_reveal_entries_below_the_fold() {
        // Final-review Critical finding — render_panel emits 2 lines per
        // tool with calls > 0 (a name line + an outcome-breakdown line),
        // but the old ScrollUp clamp used `tool_stats.len()` (the item
        // count, not the rendered line count), so the bottom rows were
        // permanently unreachable once enough tools had nonzero calls.
        use aivyx_channel::daemon_ipc::ToolStat;
        let backend = TestBackend::new(40, 8); // ~6 text rows inside the border
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.view = View::Tools;
        state.tool_stats = (0..20u64)
            .map(|i| {
                let mut outcomes = std::collections::BTreeMap::new();
                outcomes.insert("completed".to_string(), 1u64);
                ToolStat {
                    name: format!("tool.number.{i}"),
                    description: String::new(),
                    scope_base: format!("tool.number.{i}"),
                    registered: true,
                    calls: 1,
                    outcomes,
                    total_duration_ms: 5,
                }
            })
            .collect();

        terminal.draw(|f| render(f, &state)).unwrap();
        let unscrolled = buffer_text(&terminal);
        assert!(unscrolled.contains("tool.number.0 "), "first tool visible by default");
        assert!(
            !unscrolled.contains("tool.number.19"),
            "the last tool doesn't fit before scrolling"
        );

        state = crate::model::update(state, crate::model::Msg::ScrollUp(1_000));
        terminal.draw(|f| render(f, &state)).unwrap();
        let scrolled = buffer_text(&terminal);
        assert!(
            scrolled.contains("tool.number.19"),
            "scrolling to the bottom of the page must reach the last tool: {scrolled}"
        );
        assert_ne!(scrolled, unscrolled, "scrolling must actually change what's rendered");
    }

    #[test]
    fn renders_gate_prompt_when_pending() {
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut state = AppState::new();
        state.gate = Some(PendingGate {
            mission_id: "m1".into(),
            gate_id: "g1".into(),
            reason: "writes a file".into(),
            scope: Some("fs.write".into()),
        });

        terminal.draw(|f| render(f, &state)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("Approve?"), "gate prompt shown");
        assert!(text.contains("writes a file"), "gate reason shown");
        assert!(text.contains("approve"), "gate keybinding hint shown");
    }
}
