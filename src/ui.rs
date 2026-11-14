use crate::app::{App, ChildState, ExitOutcome};
use crate::store::{LogEntry, LogStore, ProcessState};
use ansi_to_tui::IntoText;
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph},
};
use std::io::{self, Stdout};
use std::sync::Once;

static PANIC_HOOK_INIT: Once = Once::new();

pub struct TerminalGuard {
    term: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;

        PANIC_HOOK_INIT.call_once(|| {
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let _ = disable_raw_mode();
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
                default_hook(info);
            }));
        });

        let backend = CrosstermBackend::new(stdout);
        let term = Terminal::new(backend)?;
        Ok(Self { term })
    }

    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.term
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.term.backend_mut(), LeaveAlternateScreen);
    }
}

fn format_entry_line(entry: &LogEntry, store: &LogStore, multiple_procs: bool) -> Line<'static> {
    if multiple_procs {
        let proc_name = store.process_name(entry.process);
        let tag = format!("[{proc_name}] ");
        if let Ok(mut text) = entry.raw_text().as_bytes().into_text() {
            if !text.lines.is_empty() {
                let first_line = text.lines.remove(0);
                let mut new_spans = vec![Span::styled(tag, Style::default().fg(Color::DarkGray))];
                new_spans.extend(first_line);
                Line::from(new_spans)
            } else {
                Line::from(tag)
            }
        } else {
            Line::from(format!("{tag}{}", entry.plain_text()))
        }
    } else {
        let sliced_raw = entry
            .raw_text()
            .get(entry.display_offset..)
            .unwrap_or(entry.raw_text());
        if let Ok(mut text) = sliced_raw.as_bytes().into_text() {
            if !text.lines.is_empty() {
                text.lines.remove(0)
            } else {
                Line::from("")
            }
        } else {
            Line::from(crate::parser::strip_ansi(sliced_raw).into_owned())
        }
    }
}

fn char_display_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    let s: &str = ch.encode_utf8(&mut buf);
    Span::raw(s).width()
}

fn wrap_styled_line(line: Line<'static>, max_cols: u16) -> Vec<Line<'static>> {
    let limit = usize::from(max_cols);
    if limit == 0 || line.width() <= limit {
        return vec![line];
    }

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut cur_spans: Vec<Span<'static>> = Vec::new();
    let mut cur_width = 0usize;

    for span in line.spans {
        let text = span.content.as_ref();
        let style = span.style;
        let mut seg_start = 0usize;

        for (byte_idx, ch) in text.char_indices() {
            let cw = char_display_width(ch);
            if cur_width + cw > limit && cur_width > 0 {
                if seg_start < byte_idx {
                    cur_spans.push(Span::styled(text[seg_start..byte_idx].to_string(), style));
                }
                rows.push(Line::from(std::mem::take(&mut cur_spans)));
                cur_width = 0;
                seg_start = byte_idx;
            }
            cur_width += cw;
        }

        if seg_start < text.len() {
            cur_spans.push(Span::styled(text[seg_start..].to_string(), style));
        }
    }

    if !cur_spans.is_empty() || rows.is_empty() {
        rows.push(Line::from(cur_spans));
    }

    rows
}

fn centered_rect(max_w: u16, max_h: u16, area: Rect) -> Rect {
    let w = max_w.min(area.width);
    let h = max_h.min(area.height);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect::new(x, y, w, h)
}

fn help_row(keys: &'static str, desc: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {keys:<17}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(desc),
    ])
}

fn render_help_popup(frame: &mut Frame<'_>) {
    let popup_area = centered_rect(68, 20, frame.area());
    frame.render_widget(Clear, popup_area);

    let block = Block::bordered()
        .title(Span::styled(
            " tmn Shortcuts (Press ? or Esc to close) ",
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let header_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let lines = vec![
        Line::from(Span::styled(" Sources", header_style)),
        help_row("j / k, Up / Down", "Move sidebar cursor (or scroll zoomed)"),
        help_row("Enter / Space", "Solo source / toggle multi-select"),
        help_row("a", "Clear source selection (All)"),
        Line::from(""),
        Line::from(Span::styled(" View & Copy", header_style)),
        help_row("z", "Zoom logs (hide sidebar for mouse scroll/copy)"),
        help_row("w", "Toggle soft line wrapping"),
        Line::from(""),
        Line::from(Span::styled(" Scroll & Filter", header_style)),
        help_row("Ctrl-U / Ctrl-D", "Scroll logs half-page up / down"),
        help_row("PgUp / PgDn", "Scroll logs full-page up / down"),
        help_row("g / G", "Oldest line / tail-follow newest"),
        help_row("1 - 5", "Min severity (ALL, DEBUG+, INFO+, WARN+, ERR+)"),
        help_row("/ , Esc", "Regex search (smart-case) / clear regex"),
        Line::from(""),
        Line::from(Span::styled(" Lifecycle", header_style)),
        help_row("Ctrl-C / q", "Stop child & hold logs / quit cleanly"),
    ];

    frame.render_widget(Paragraph::new(lines).block(block), popup_area);
}

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let (top_area, main_area, bottom_area) = (chunks[0], chunks[1], chunks[2]);

    let log_area = if app.sidebar_visible() {
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(28), Constraint::Min(1)])
            .split(main_area);

        let sidebar_area = main_chunks[0];
        let logs_area = main_chunks[1];

        let mut items = Vec::new();
        for (i, row) in app.store().source_rows().into_iter().enumerate() {
            let sel_marker = if let Some(t) = row.target {
                if app.filter().selected_sources().contains(&t) {
                    "*"
                } else {
                    " "
                }
            } else if app.filter().selected_sources().is_empty() {
                "*"
            } else {
                " "
            };

            let status_dot = match row.state {
                ProcessState::Running { .. } => "●",
                ProcessState::Exited { code: Some(0) } => "✓",
                ProcessState::Exited { .. } => "✗",
                ProcessState::Passive => "·",
            };

            let indent = " ".repeat(row.depth as usize * 2);
            let mut label = format!("{sel_marker} {indent}{status_dot} {}", row.label);

            if row.warn_count > 0 || row.error_count > 0 {
                label.push_str(&format!(" (!{} ×{})", row.warn_count, row.error_count));
            }

            let style = if i == app.selected_row {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            items.push(ListItem::new(label).style(style));
        }

        let mut list_state = ListState::default();
        list_state.select(Some(app.selected_row));
        frame.render_stateful_widget(List::new(items), sidebar_area, &mut list_state);

        logs_area
    } else {
        main_area
    };

    app.last_log_viewport_size = (log_area.width, log_area.height);

    let multiple_procs = app.filter().shows_multiple_processes(app.store());
    let filtered_indices = app.store().filtered_indices(app.filter());

    let h = log_area.height as usize;
    let entries = app.store().entries();
    let mut log_lines = Vec::new();

    if h > 0 && !filtered_indices.is_empty() {
        if !app.wrap_lines() {
            let top_idx = if app.follow_tail() {
                filtered_indices.len().saturating_sub(h)
            } else {
                let anchor = app.anchor_seq.unwrap_or(0);
                filtered_indices
                    .partition_point(|&v| entries.get(v).is_some_and(|e| e.seq < anchor))
            };
            let visible_indices =
                &filtered_indices[top_idx..std::cmp::min(top_idx + h, filtered_indices.len())];
            for &idx in visible_indices {
                if let Some(entry) = entries.get(idx) {
                    log_lines.push(format_entry_line(entry, app.store(), multiple_procs));
                }
            }
        } else if app.follow_tail() {
            let mut rev_chunks: Vec<Vec<Line<'static>>> = Vec::new();
            let mut total_rows = 0usize;
            for &idx in filtered_indices.iter().rev() {
                if let Some(entry) = entries.get(idx) {
                    let wrapped = wrap_styled_line(
                        format_entry_line(entry, app.store(), multiple_procs),
                        log_area.width,
                    );
                    total_rows += wrapped.len();
                    rev_chunks.push(wrapped);
                    if total_rows >= h {
                        break;
                    }
                }
            }
            let skip_top = total_rows.saturating_sub(h);
            log_lines = rev_chunks
                .into_iter()
                .rev()
                .flatten()
                .skip(skip_top)
                .collect();
        } else {
            let anchor = app.anchor_seq.unwrap_or(0);
            let top_idx = filtered_indices
                .partition_point(|&v| entries.get(v).is_some_and(|e| e.seq < anchor));
            for &idx in &filtered_indices[top_idx..] {
                if let Some(entry) = entries.get(idx) {
                    let wrapped = wrap_styled_line(
                        format_entry_line(entry, app.store(), multiple_procs),
                        log_area.width,
                    );
                    log_lines.extend(wrapped);
                    if log_lines.len() >= h {
                        log_lines.truncate(h);
                        break;
                    }
                }
            }
        }
    }

    frame.render_widget(Paragraph::new(log_lines), log_area);

    let (state_text, state_color) = match app.child_state() {
        ChildState::Running => ("RUNNING".to_string(), Color::Green),
        ChildState::Stopping { .. } => ("STOPPING...".to_string(), Color::Yellow),
        ChildState::Quitting { .. } => ("QUITTING...".to_string(), Color::Yellow),
        ChildState::Exited(ExitOutcome::Code(0)) => ("EXITED (0)".to_string(), Color::DarkGray),
        ChildState::Exited(ExitOutcome::Code(c)) => (format!("CRASHED ({})", c), Color::Red),
        ChildState::Exited(ExitOutcome::Signal(s)) => (format!("CRASHED (sig {})", s), Color::Red),
    };

    let mut top_spans = Vec::new();
    if !app.cmd_name().is_empty() {
        top_spans.push(Span::styled(
            format!("{} | ", app.cmd_name()),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    top_spans.push(Span::styled(
        state_text,
        Style::default()
            .fg(state_color)
            .add_modifier(Modifier::BOLD),
    ));

    let paused_str = if app.follow_tail() {
        ""
    } else {
        " [PAUSED — press G to follow]"
    };
    let sev_str = app
        .filter()
        .min_severity()
        .map(|s| s.as_str())
        .unwrap_or("ALL");
    let regex_str = if app.filter().query_text().is_empty() {
        ""
    } else {
        " REGEX"
    };
    let wrap_str = if app.wrap_lines() { " [WRAP]" } else { "" };
    top_spans.push(Span::styled(
        format!(
            " | Sev: {}{}{}| {}/{}{}",
            sev_str,
            regex_str,
            wrap_str,
            filtered_indices.len(),
            entries.len(),
            paused_str
        ),
        Style::default().add_modifier(Modifier::BOLD),
    ));

    frame.render_widget(Paragraph::new(Line::from(top_spans)), top_area);

    let bottom_str = if app.is_searching {
        if let Some(err) = app.filter().regex_error() {
            Line::from(vec![
                Span::styled(
                    format!("/{} ", app.filter().query_text()),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("(Invalid Regex: {})", err),
                    Style::default().fg(Color::Yellow),
                ),
            ])
        } else {
            Line::from(format!("/{}", app.filter().query_text()))
        }
    } else {
        Line::from("Enter/Spc: Filter | z: Zoom | w: Wrap | 1-5: Sev | /: Find | ?: Help | q: Quit")
    };
    frame.render_widget(Paragraph::new(bottom_str), bottom_area);

    if app.show_help() {
        render_help_popup(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn test_ui_snapshot() {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("cmd".into(), 100);
        app.on_line("[proc-1] \x1b[31m[ERROR] [logger1]: boom\x1b[0m");

        terminal.draw(|f| render(f, &mut app)).unwrap();

        let _buffer = terminal.backend().buffer();
        assert!(app.last_log_viewport_size.0 > 0);
        assert!(app.last_log_viewport_size.1 > 0);
    }

    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn test_fallback_slice_safety_with_utf8_ansi() {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("cmd".into(), 100);
        app.on_key(key(KeyCode::Char('z'))); // hide sidebar

        // Test slicing fallback safety
        app.on_line("\x1b[31m[proc-1] café\x1b[0m");

        terminal.draw(|f| render(f, &mut app)).unwrap();
        assert!(app.last_log_viewport_size.1 > 0);
    }

    #[test]
    fn test_status_bar_exit_codes() {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("ros2 launch test".into(), 100);

        // 1. RUNNING (default, Green)
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let line = (0..100)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect::<String>();
        assert!(line.contains("ros2 launch test"));
        assert!(line.contains("RUNNING"));
        let run_idx = line.find("RUNNING").unwrap();
        assert_eq!(buf.cell((run_idx as u16, 0)).unwrap().fg, Color::Green);

        // 2. EXITED (0) (DarkGray)
        app.on_child_exited(ExitOutcome::Code(0));
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let line = (0..100)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect::<String>();
        assert!(line.contains("ros2 launch test"));
        assert!(line.contains("EXITED (0)"));
        let exit_idx = line.find("EXITED (0)").unwrap();
        assert_eq!(buf.cell((exit_idx as u16, 0)).unwrap().fg, Color::DarkGray);

        // 3. CRASHED (code) (Red)
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let mut app = App::new("ros2 launch test".into(), 100);
        app.on_child_exited(ExitOutcome::Code(-9));
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let line = (0..100)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect::<String>();
        assert!(line.contains("CRASHED (-9)"));
        let crash_idx = line.find("CRASHED (-9)").unwrap();
        assert_eq!(buf.cell((crash_idx as u16, 0)).unwrap().fg, Color::Red);

        // 4. CRASHED (signal) (Red)
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let mut app = App::new("ros2 launch test".into(), 100);
        app.on_child_exited(ExitOutcome::Signal(2));
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let line = (0..100)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect::<String>();
        assert!(line.contains("CRASHED (sig 2)"));
        let sig_idx = line.find("CRASHED (sig 2)").unwrap();
        assert_eq!(buf.cell((sig_idx as u16, 0)).unwrap().fg, Color::Red);
    }

    #[test]
    fn test_invalid_regex_highlight() {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("cmd".into(), 100);

        app.on_key(key(KeyCode::Char('/')));
        for c in "[unclosed".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }

        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();

        let bottom_y = 19;
        let line = (0..100)
            .map(|x| buffer.cell((x, bottom_y)).unwrap().symbol().to_string())
            .collect::<String>();
        assert!(line.contains("/[unclosed"));
        assert!(line.contains("Invalid Regex"));
        assert_eq!(
            buffer.cell((0, bottom_y)).unwrap().fg,
            ratatui::style::Color::Yellow
        );
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let area = buf.area;
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_bottom_bar_includes_zoom_wrap_and_help_within_80_cols() {
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("cmd".into(), 100);

        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let bottom = (0..80)
            .map(|x| buf.cell((x, 19)).unwrap().symbol())
            .collect::<String>();

        assert!(bottom.contains("z: Zoom"), "bottom={bottom}");
        assert!(bottom.contains("w: Wrap"), "bottom={bottom}");
        assert!(bottom.contains("?: Help"), "bottom={bottom}");
        assert!(bottom.contains("q: Quit"), "bottom={bottom}");
    }

    #[test]
    fn test_wrap_lines_preserves_tail_follow_and_shows_wrap_badge() {
        // 30 cols x 6 rows => 1 top bar, 4 log rows (y=1..=4), 1 bottom bar
        let backend = TestBackend::new(30, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(String::new(), 100);
        app.on_key(key(KeyCode::Char('z'))); // zoom full 30 cols
        app.on_key(key(KeyCode::Char('w'))); // enable line wrap

        app.on_line("LINE_ONE");
        app.on_line("LINE_TWO");
        // 68 chars -> wraps into 3 visual rows on a 30-col viewport (30 + 30 + 8)
        app.on_line("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAABBBBBBBBBBBBBBBBBBBBBBBBBBBBBBWRAP_END");
        app.on_line("FINAL_BOTTOM_ROW");

        terminal.draw(|f| render(f, &mut app)).unwrap();
        let screen = buffer_text(&terminal);

        assert!(screen.contains("[WRAP]"), "missing [WRAP] badge:\n{screen}");
        assert!(
            screen.contains("WRAP_END"),
            "wrapped tail pushed off screen:\n{screen}"
        );
        assert!(
            screen.contains("FINAL_BOTTOM_ROW"),
            "newest line pushed off screen:\n{screen}"
        );

        // Jumping to top ('g') while wrapped renders from the oldest entry forward
        app.on_key(key(KeyCode::Char('g')));
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let paused_screen = buffer_text(&terminal);
        assert!(
            paused_screen.contains("LINE_ONE"),
            "paused:\n{paused_screen}"
        );
        assert!(
            paused_screen.contains("LINE_TWO"),
            "paused:\n{paused_screen}"
        );
    }

    #[test]
    fn test_wrap_styled_line_preserves_ansi_style_and_multibyte_widths() {
        let backend = TestBackend::new(16, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("cmd".into(), 100);
        app.on_key(key(KeyCode::Char('z')));
        app.on_key(key(KeyCode::Char('w')));

        // 15 ASCII chars + 2-col emoji 🚀 => 🚀 must wrap cleanly to row y=2 with Red style intact
        app.on_line("\x1b[31mABCDEFGHIJKLMNO🚀XYZ\x1b[0m");

        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // Row y=2: cell 0 is 2-col '🚀', cell 2 is 'X', both preserving Color::Red
        assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "🚀");
        assert_eq!(buf.cell((0, 2)).unwrap().fg, Color::Red);
        assert_eq!(buf.cell((2, 2)).unwrap().symbol(), "X");
        assert_eq!(buf.cell((2, 2)).unwrap().fg, Color::Red);
    }

    #[test]
    fn test_help_popup_renders_and_closes_on_toggle() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new("cmd".into(), 100);
        app.on_key(key(KeyCode::Char('?')));

        terminal.draw(|f| render(f, &mut app)).unwrap();
        let screen = buffer_text(&terminal);
        assert!(screen.contains("Ctrl-U / Ctrl-D"), "screen:\n{screen}");
        assert!(screen.contains("PgUp / PgDn"), "screen:\n{screen}");
        assert!(
            screen.contains("Press ? or Esc to close"),
            "screen:\n{screen}"
        );

        app.on_key(key(KeyCode::Char('?')));
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let closed_screen = buffer_text(&terminal);
        assert!(!closed_screen.contains("Press ? or Esc to close"));
    }
}
