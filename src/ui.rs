use crate::app::{App, ChildState, ExitOutcome};
use crate::store::ProcessState;
use ansi_to_tui::IntoText;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame, Terminal,
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
    let top_idx = if app.follow_tail() {
        filtered_indices.len().saturating_sub(h)
    } else {
        let anchor = app.anchor_seq.unwrap_or(0);
        let entries = app.store().entries();
        filtered_indices.partition_point(|&v| entries.get(v).is_some_and(|e| e.seq < anchor))
    };
    
    let visible_indices = &filtered_indices[top_idx..std::cmp::min(top_idx + h, filtered_indices.len())];
    
    let mut log_lines = Vec::new();
    let entries = app.store().entries();
    for &idx in visible_indices {
        if let Some(entry) = entries.get(idx) {
            let line = if multiple_procs {
                let proc_name = app.store().process_name(entry.process);
                let tag = format!("[{}] ", proc_name);
                if let Ok(mut text) = entry.raw_text().as_bytes().into_text() {
                    if !text.lines.is_empty() {
                        let first_line = text.lines.remove(0);
                        let mut new_spans = vec![Span::styled(tag, Style::default().fg(Color::DarkGray))];
                        new_spans.extend(first_line.into_iter());
                        Line::from(new_spans)
                    } else {
                        Line::from(tag)
                    }
                } else {
                    Line::from(format!("{}{}", tag, entry.plain_text()))
                }
            } else {
                let sliced_raw = entry.raw_text().get(entry.display_offset..).unwrap_or(entry.raw_text());
                if let Ok(mut text) = sliced_raw.as_bytes().into_text() {
                    if !text.lines.is_empty() {
                        text.lines.remove(0)
                    } else {
                        Line::from("")
                    }
                } else {
                    Line::from(crate::parser::strip_ansi(sliced_raw).into_owned())
                }
            };
            log_lines.push(line);
        }
    }

    frame.render_widget(Paragraph::new(log_lines), log_area);

    let state_str = match app.child_state() {
        ChildState::Running => "RUNNING".to_string(),
        ChildState::Stopping { .. } => "STOPPING...".to_string(),
        ChildState::Quitting { .. } => "QUITTING...".to_string(),
        ChildState::Exited(ExitOutcome::Code(0)) => "EXITED (0)".to_string(),
        ChildState::Exited(ExitOutcome::Code(c)) => format!("CRASHED ({})", c),
        ChildState::Exited(ExitOutcome::Signal(s)) => format!("CRASHED (sig {})", s),
    };
    let paused_str = if app.follow_tail() { "" } else { " [PAUSED — press G to follow]" };
    let sev_str = app.filter().min_severity().map(|s| s.as_str()).unwrap_or("ALL");
    let regex_str = if app.filter().query_text().is_empty() { "" } else { " REGEX" };
    let top_str = format!("{} | Sev: {}{} | {}/{} {}", state_str, sev_str, regex_str, filtered_indices.len(), entries.len(), paused_str);
    frame.render_widget(Paragraph::new(top_str).style(Style::default().add_modifier(Modifier::BOLD)), top_area);

    let bottom_str = if app.is_searching {
        if let Some(err) = app.filter().regex_error() {
            Line::from(vec![
                Span::styled(format!("/{} ", app.filter().query_text()), Style::default().fg(Color::Yellow)),
                Span::styled(format!("(Invalid Regex: {})", err), Style::default().fg(Color::Yellow)),
            ])
        } else {
            Line::from(format!("/{}", app.filter().query_text()))
        }
    } else {
        Line::from("j/k: Navigate | Enter: Solo | Space: Multi | a: All | 1-5: Sev | /: Search | q: Quit")
    };
    frame.render_widget(Paragraph::new(bottom_str), bottom_area);
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
        let mut app = App::new("cmd".into(), 100);
        
        app.on_child_exited(ExitOutcome::Code(0));
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let line = (0..100).map(|x| terminal.backend().buffer().cell((x, 0)).unwrap().symbol().to_string()).collect::<String>();
        assert!(line.contains("EXITED (0)"));

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let mut app = App::new("cmd".into(), 100);
        app.on_child_exited(ExitOutcome::Code(-9));
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let line = (0..100).map(|x| terminal.backend().buffer().cell((x, 0)).unwrap().symbol().to_string()).collect::<String>();
        assert!(line.contains("CRASHED (-9)"));

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let mut app = App::new("cmd".into(), 100);
        app.on_child_exited(ExitOutcome::Signal(2));
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let line = (0..100).map(|x| terminal.backend().buffer().cell((x, 0)).unwrap().symbol().to_string()).collect::<String>();
        assert!(line.contains("CRASHED (sig 2)"));
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
        let line = (0..100).map(|x| buffer.cell((x, bottom_y)).unwrap().symbol().to_string()).collect::<String>();
        assert!(line.contains("/[unclosed"));
        assert!(line.contains("Invalid Regex"));
        assert_eq!(buffer.cell((0, bottom_y)).unwrap().fg, ratatui::style::Color::Yellow);
    }

}
