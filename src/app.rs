use crate::parser::Severity;
use crate::store::{FilterState, LogStore};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitOutcome {
    Code(i32),
    Signal(i32),
}

impl ExitOutcome {
    #[must_use]
    pub fn shell_exit_code(self) -> i32 {
        match self {
            Self::Code(c) => c,
            Self::Signal(s) => 128 + s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitStage {
    IntSent,
    TermSent,
    KillSent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildState {
    Running,
    Stopping { interrupts_sent: u8 },
    Quitting { stage: QuitStage },
    Exited(ExitOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalReq {
    Int,
    Term,
    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    None,
    SignalChild(SignalReq),
    ExitTui,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpMode {
    Closed,
    HeldOpen,
    ToggledOpen,
}

fn is_help_trigger(key: &KeyEvent) -> bool {
    let disallowed = KeyModifiers::CONTROL | KeyModifiers::ALT;
    if key.modifiers.intersects(disallowed) {
        return false;
    }
    key.code == KeyCode::Char('?')
        || (key.code == KeyCode::Char('/') && key.modifiers.contains(KeyModifiers::SHIFT))
}

pub struct App {
    cmd_name: String,
    store: LogStore,
    filter: FilterState,
    child_state: ChildState,
    sidebar_visible: bool,
    wrap_lines: bool,
    follow_tail: bool,
    help_mode: HelpMode,
    supports_key_release: bool,
    pub last_log_viewport_size: (u16, u16),
    pub selected_row: usize,
    pub anchor_seq: Option<u64>,
    pub is_searching: bool,
}

impl App {
    #[must_use]
    pub fn new(cmd_name: String, max_lines: usize) -> Self {
        Self {
            cmd_name,
            store: LogStore::new(max_lines),
            filter: FilterState::new(),
            child_state: ChildState::Running,
            sidebar_visible: true,
            wrap_lines: false,
            follow_tail: true,
            help_mode: HelpMode::Closed,
            supports_key_release: false,
            last_log_viewport_size: (0, 0),
            selected_row: 0,
            anchor_seq: None,
            is_searching: false,
        }
    }

    #[must_use]
    pub fn cmd_name(&self) -> &str {
        &self.cmd_name
    }
    #[must_use]
    pub fn store(&self) -> &LogStore {
        &self.store
    }
    #[must_use]
    pub fn filter(&self) -> &FilterState {
        &self.filter
    }
    #[must_use]
    pub fn child_state(&self) -> ChildState {
        self.child_state
    }
    #[must_use]
    pub fn sidebar_visible(&self) -> bool {
        self.sidebar_visible
    }
    #[must_use]
    pub fn wrap_lines(&self) -> bool {
        self.wrap_lines
    }
    #[must_use]
    pub fn follow_tail(&self) -> bool {
        self.follow_tail
    }
    #[must_use]
    pub fn help_mode(&self) -> HelpMode {
        self.help_mode
    }
    #[must_use]
    pub fn supports_key_release(&self) -> bool {
        self.supports_key_release
    }
    pub fn set_supports_key_release(&mut self, supported: bool) {
        self.supports_key_release = supported;
    }
    #[must_use]
    pub fn last_log_viewport_size(&self) -> (u16, u16) {
        self.last_log_viewport_size
    }

    pub fn on_line(&mut self, line: &str) {
        self.store.ingest_line(line);
    }

    pub fn on_child_exited(&mut self, outcome: ExitOutcome) -> AppAction {
        match self.child_state {
            ChildState::Running | ChildState::Stopping { .. } => {
                self.child_state = ChildState::Exited(outcome);
                AppAction::None
            }
            ChildState::Quitting { .. } => {
                self.child_state = ChildState::Exited(outcome);
                AppAction::ExitTui
            }
            ChildState::Exited(_) => AppAction::None,
        }
    }

    fn escalate_quit(&mut self, stage: QuitStage) -> AppAction {
        let (new_stage, req) = match stage {
            QuitStage::IntSent => (QuitStage::TermSent, SignalReq::Term),
            QuitStage::TermSent | QuitStage::KillSent => (QuitStage::KillSent, SignalReq::Kill),
        };
        self.child_state = ChildState::Quitting { stage: new_stage };
        AppAction::SignalChild(req)
    }

    pub fn on_quit_timeout(&mut self) -> AppAction {
        match self.child_state {
            ChildState::Quitting { stage } => self.escalate_quit(stage),
            _ => AppAction::None,
        }
    }

    pub fn request_external_shutdown(&mut self) -> AppAction {
        match self.child_state {
            ChildState::Exited(_) => AppAction::ExitTui,
            ChildState::Running => {
                self.child_state = ChildState::Quitting {
                    stage: QuitStage::IntSent,
                };
                AppAction::SignalChild(SignalReq::Int)
            }
            ChildState::Stopping { .. } | ChildState::Quitting { .. } => self.on_quit_timeout(),
        }
    }

    pub fn scroll_up(&mut self, lines: usize) {
        let indices = self.store.filtered_indices(&self.filter);
        if indices.is_empty() {
            return;
        }
        let h = self.last_log_viewport_size.1 as usize;

        let top_idx = if self.follow_tail {
            indices.len().saturating_sub(h)
        } else {
            let current_anchor = self.anchor_seq.unwrap_or(0);
            let entries = self.store.entries();
            // Binary search: `entries` is sorted by seq. `filtered_indices` is also sorted.
            indices.partition_point(|&v| entries.get(v).is_some_and(|e| e.seq < current_anchor))
        };

        self.follow_tail = false;
        let new_idx = top_idx.saturating_sub(lines);
        let actual = indices[new_idx];
        self.anchor_seq = Some(self.store.entries()[actual].seq);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        if self.follow_tail {
            return;
        }
        let indices = self.store.filtered_indices(&self.filter);
        if indices.is_empty() {
            return;
        }

        let current_anchor = self.anchor_seq.unwrap_or(0);
        let entries = self.store.entries();
        let top_idx =
            indices.partition_point(|&v| entries.get(v).is_some_and(|e| e.seq < current_anchor));

        let h = self.last_log_viewport_size.1 as usize;
        let max_top = indices.len().saturating_sub(h.max(1));
        let new_idx = std::cmp::min(top_idx + lines, max_top);
        if new_idx >= max_top {
            self.follow_tail = true;
            self.anchor_seq = None;
        } else {
            let actual = indices[new_idx];
            self.anchor_seq = Some(entries[actual].seq);
        }
    }

    fn handle_ctrl_c(&mut self) -> AppAction {
        match self.child_state {
            ChildState::Running => {
                self.child_state = ChildState::Stopping { interrupts_sent: 1 };
                AppAction::SignalChild(SignalReq::Int)
            }
            ChildState::Stopping { interrupts_sent } => {
                let new_sent = interrupts_sent.saturating_add(1);
                self.child_state = ChildState::Stopping {
                    interrupts_sent: new_sent,
                };
                if new_sent >= 3 {
                    AppAction::SignalChild(SignalReq::Term)
                } else {
                    AppAction::SignalChild(SignalReq::Int)
                }
            }
            ChildState::Quitting { stage } => self.escalate_quit(stage),
            ChildState::Exited(_) => AppAction::None,
        }
    }

    fn handle_quit(&mut self) -> AppAction {
        match self.child_state {
            ChildState::Running => {
                self.child_state = ChildState::Quitting {
                    stage: QuitStage::IntSent,
                };
                AppAction::SignalChild(SignalReq::Int)
            }
            ChildState::Stopping { .. } => {
                self.child_state = ChildState::Quitting {
                    stage: QuitStage::TermSent,
                };
                AppAction::SignalChild(SignalReq::Term)
            }
            ChildState::Quitting { stage } => self.escalate_quit(stage),
            ChildState::Exited(_) => AppAction::ExitTui,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> AppAction {
        if self.is_searching {
            if key.kind == KeyEventKind::Release {
                return AppAction::None;
            }
            match key.code {
                KeyCode::Esc => {
                    self.is_searching = false;
                    self.filter.clear_regex();
                }
                KeyCode::Enter => {
                    self.is_searching = false;
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let _ = self.filter.set_regex_query("");
                }
                KeyCode::Char(c) => {
                    let mut q = self.filter.query_text().to_string();
                    q.push(c);
                    let _ = self.filter.set_regex_query(&q);
                }
                KeyCode::Backspace => {
                    let mut q = self.filter.query_text().to_string();
                    q.pop();
                    let _ = self.filter.set_regex_query(&q);
                }
                _ => {}
            }
            return AppAction::None;
        }

        if self.help_mode != HelpMode::Closed {
            if key.kind == KeyEventKind::Release {
                if self.help_mode == HelpMode::HeldOpen
                    && matches!(key.code, KeyCode::Char('?') | KeyCode::Char('/'))
                {
                    self.help_mode = HelpMode::Closed;
                }
                return AppAction::None;
            }
            if key.kind == KeyEventKind::Press {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return self.handle_ctrl_c();
                }
                if key.code == KeyCode::Char('q') && key.modifiers.is_empty() {
                    return self.handle_quit();
                }
                if key.code == KeyCode::Esc
                    || (self.help_mode == HelpMode::ToggledOpen && is_help_trigger(&key))
                {
                    self.help_mode = HelpMode::Closed;
                }
            }
            return AppAction::None;
        }

        if key.kind == KeyEventKind::Release {
            return AppAction::None;
        }

        if key.kind == KeyEventKind::Press && is_help_trigger(&key) {
            self.help_mode = if self.supports_key_release {
                HelpMode::HeldOpen
            } else {
                HelpMode::ToggledOpen
            };
            return AppAction::None;
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.handle_ctrl_c()
            }
            KeyCode::Char('q') => self.handle_quit(),
            KeyCode::Esc => {
                self.filter.clear_regex();
                AppAction::None
            }
            KeyCode::Char('g') => {
                let indices = self.store.filtered_indices(&self.filter);
                if indices.is_empty() {
                    return AppAction::None;
                }

                self.follow_tail = false;
                let actual = indices[0];
                self.anchor_seq = Some(self.store.entries()[actual].seq);
                AppAction::None
            }
            KeyCode::Char('G') => {
                self.follow_tail = true;
                self.anchor_seq = None;
                AppAction::None
            }
            KeyCode::Char('z') => {
                self.sidebar_visible = !self.sidebar_visible;
                AppAction::None
            }
            KeyCode::Char('w') => {
                self.wrap_lines = !self.wrap_lines;
                AppAction::None
            }
            KeyCode::Char('a') => {
                self.filter.clear_sources();
                AppAction::None
            }
            KeyCode::Char('1') => {
                self.filter.set_min_severity(None);
                AppAction::None
            }
            KeyCode::Char('2') => {
                self.filter.set_min_severity(Some(Severity::Debug));
                AppAction::None
            }
            KeyCode::Char('3') => {
                self.filter.set_min_severity(Some(Severity::Info));
                AppAction::None
            }
            KeyCode::Char('4') => {
                self.filter.set_min_severity(Some(Severity::Warn));
                AppAction::None
            }
            KeyCode::Char('5') => {
                self.filter.set_min_severity(Some(Severity::Error));
                AppAction::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.sidebar_visible {
                    let rows = self.store.source_rows().len();
                    if rows > 0 && self.selected_row < rows - 1 {
                        self.selected_row += 1;
                    }
                } else {
                    self.scroll_down(1);
                }
                AppAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.sidebar_visible {
                    if self.selected_row > 0 {
                        self.selected_row -= 1;
                    }
                } else {
                    self.scroll_up(1);
                }
                AppAction::None
            }
            KeyCode::PageUp => {
                let h = self.last_log_viewport_size.1 as usize;
                self.scroll_up(h.max(1));
                AppAction::None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let h = self.last_log_viewport_size.1 as usize;
                self.scroll_up((h / 2).max(1));
                AppAction::None
            }
            KeyCode::PageDown => {
                let h = self.last_log_viewport_size.1 as usize;
                self.scroll_down(h.max(1));
                AppAction::None
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let h = self.last_log_viewport_size.1 as usize;
                self.scroll_down((h / 2).max(1));
                AppAction::None
            }
            KeyCode::Enter => {
                if self.sidebar_visible {
                    let rows = self.store.source_rows();
                    if let Some(row) = rows.get(self.selected_row) {
                        self.filter.solo(row.target);
                    }
                }
                AppAction::None
            }
            KeyCode::Char(' ') => {
                if self.sidebar_visible {
                    let rows = self.store.source_rows();
                    if let Some(row) = rows.get(self.selected_row) {
                        if row.target.is_none() {
                            self.filter.clear_sources();
                        } else {
                            self.filter.toggle(row.target);
                        }
                    }
                }
                AppAction::None
            }
            KeyCode::Char('/') => {
                self.is_searching = true;
                AppAction::None
            }
            _ => AppAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }
    fn key_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn test_lifecycle_and_shutdown_transitions() {
        let mut app = App::new("cmd".into(), 100);

        let action = app.on_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(action, AppAction::SignalChild(SignalReq::Int));
        assert_eq!(app.child_state, ChildState::Stopping { interrupts_sent: 1 });

        let action2 = app.on_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(action2, AppAction::SignalChild(SignalReq::Int));
        assert_eq!(app.child_state, ChildState::Stopping { interrupts_sent: 2 });

        let action3 = app.on_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(action3, AppAction::SignalChild(SignalReq::Term));
        assert_eq!(app.child_state, ChildState::Stopping { interrupts_sent: 3 });

        let action_exited = app.on_child_exited(ExitOutcome::Signal(2));
        assert_eq!(action_exited, AppAction::None); // Post-mortem hold
        assert_eq!(app.child_state, ChildState::Exited(ExitOutcome::Signal(2)));

        let action_q = app.on_key(key(KeyCode::Char('q')));
        assert_eq!(action_q, AppAction::ExitTui);
    }

    #[test]
    fn test_lifecycle_quit_flow() {
        let mut app = App::new("cmd".into(), 100);

        let action = app.on_key(key(KeyCode::Char('q')));
        assert_eq!(action, AppAction::SignalChild(SignalReq::Int));
        assert_eq!(
            app.child_state,
            ChildState::Quitting {
                stage: QuitStage::IntSent
            }
        );

        let action2 = app.on_quit_timeout();
        assert_eq!(action2, AppAction::SignalChild(SignalReq::Term));
        assert_eq!(
            app.child_state,
            ChildState::Quitting {
                stage: QuitStage::TermSent
            }
        );

        let action3 = app.on_quit_timeout();
        assert_eq!(action3, AppAction::SignalChild(SignalReq::Kill));
        assert_eq!(
            app.child_state,
            ChildState::Quitting {
                stage: QuitStage::KillSent
            }
        );

        let action_exited = app.on_child_exited(ExitOutcome::Code(0));
        assert_eq!(action_exited, AppAction::ExitTui);
    }

    #[test]
    fn paused_scroll_anchor_compensates_for_ring_buffer_eviction() {
        let mut app = App::new("cmd".into(), 5);
        app.on_line("L1");
        app.on_line("L2");
        app.on_line("L3");
        app.on_line("L4");
        app.on_line("L5");

        app.follow_tail = false;
        app.anchor_seq = Some(2); // L3 (seq is 0-indexed, so seq=2 is L3)

        app.on_line("L6"); // Evicts L1

        assert_eq!(app.anchor_seq, Some(2));

        app.on_key(key(KeyCode::Char('G')));
        assert!(app.follow_tail);
        assert_eq!(app.anchor_seq, None);
    }

    #[test]
    fn scroll_down_clamps_and_reengages_follow_tail() {
        let mut app = App::new("cmd".into(), 10);
        for i in 0..10 {
            app.on_line(&format!("L{}", i));
        }
        app.last_log_viewport_size = (80, 5);

        app.follow_tail = false;
        app.anchor_seq = Some(3);

        app.scroll_down(100);

        assert!(app.follow_tail);
        assert_eq!(app.anchor_seq, None);

        app.follow_tail = false;
        app.anchor_seq = Some(3);
        app.scroll_down(2);
        assert!(app.follow_tail);
        assert_eq!(app.anchor_seq, None);

        app.follow_tail = false;
        app.anchor_seq = Some(3);
        app.scroll_down(1);
        assert!(!app.follow_tail);
        assert_eq!(app.anchor_seq, Some(4));
    }

    #[test]
    fn test_app_cmd_name() {
        let app = App::new("ros2 launch my_pkg my.launch.py".into(), 100);
        assert_eq!(app.cmd_name(), "ros2 launch my_pkg my.launch.py");
    }

    fn key_kind(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn test_wrap_toggle_key_and_search_isolation() {
        let mut app = App::new("cmd".into(), 100);
        assert!(!app.wrap_lines());

        app.on_key(key(KeyCode::Char('w')));
        assert!(app.wrap_lines());

        // Key release should never toggle wrap_lines back off
        app.on_key(key_kind(
            KeyCode::Char('w'),
            KeyModifiers::empty(),
            KeyEventKind::Release,
        ));
        assert!(app.wrap_lines());

        // Typing 'w' inside '/' search appends to query instead of toggling wrap_lines
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Char('w')));
        assert!(app.wrap_lines());
        assert_eq!(app.filter().query_text(), "w");
        app.on_key(key(KeyCode::Enter));

        app.on_key(key(KeyCode::Char('w')));
        assert!(!app.wrap_lines());
    }

    #[test]
    fn test_help_hold_to_peek_when_key_release_supported() {
        let mut app = App::new("cmd".into(), 100);
        app.set_supports_key_release(true);
        assert_eq!(app.help_mode(), HelpMode::Closed);

        // Press '?' opens HeldOpen
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.help_mode(), HelpMode::HeldOpen);

        // Auto-repeat while holding '?' keeps it HeldOpen
        app.on_key(key_kind(
            KeyCode::Char('?'),
            KeyModifiers::empty(),
            KeyEventKind::Repeat,
        ));
        assert_eq!(app.help_mode(), HelpMode::HeldOpen);

        // Filter keys are ignored while help popup is open
        app.on_key(key(KeyCode::Char('4')));
        assert_eq!(app.filter().min_severity(), None);

        // Releasing Shift slightly before '/' emits KeyCode::Char('/') on Release -> must close!
        app.on_key(key_kind(
            KeyCode::Char('/'),
            KeyModifiers::empty(),
            KeyEventKind::Release,
        ));
        assert_eq!(app.help_mode(), HelpMode::Closed);

        // Shift + '/' press also opens HeldOpen, and releasing '?' closes it
        app.on_key(key_mod(KeyCode::Char('/'), KeyModifiers::SHIFT));
        assert_eq!(app.help_mode(), HelpMode::HeldOpen);
        app.on_key(key_kind(
            KeyCode::Char('?'),
            KeyModifiers::SHIFT,
            KeyEventKind::Release,
        ));
        assert_eq!(app.help_mode(), HelpMode::Closed);

        // Esc also closes HeldOpen if terminal loses focus before key release
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.help_mode(), HelpMode::HeldOpen);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.help_mode(), HelpMode::Closed);
    }

    #[test]
    fn test_help_toggle_fallback_when_key_release_unsupported() {
        let mut app = App::new("cmd".into(), 100);
        assert!(!app.supports_key_release());
        assert_eq!(app.help_mode(), HelpMode::Closed);

        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.help_mode(), HelpMode::ToggledOpen);

        // Filter keys ignored while ToggledOpen
        app.on_key(key(KeyCode::Char('5')));
        assert_eq!(app.filter().min_severity(), None);

        // Pressing '?' again toggles it closed
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.help_mode(), HelpMode::Closed);

        // Pressing Esc also closes ToggledOpen
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.help_mode(), HelpMode::ToggledOpen);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.help_mode(), HelpMode::Closed);

        // Emergency quit ('q') still works even while help popup is open
        app.on_child_exited(ExitOutcome::Code(0));
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.help_mode(), HelpMode::ToggledOpen);
        assert_eq!(app.on_key(key(KeyCode::Char('q'))), AppAction::ExitTui);
    }
}
