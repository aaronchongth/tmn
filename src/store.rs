use std::collections::{HashMap, HashSet, VecDeque};
use regex::Regex;

use crate::parser::{parse_line, LifecycleEvent, LineKind, Severity};

/// Opaque interned identifier for an operating system process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(pub u16);

/// Opaque interned identifier for a ROS/logging node logger name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LoggerId(pub u16);

/// Process lifecycle state extracted from launch lifecycle events and prefix observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Discovered passively without explicit PID.
    Passive,
    /// Actively running with optional OS PID.
    Running { pid: Option<u32> },
    /// Process has terminated with optional exit code.
    Exited { code: Option<i32> },
}

/// Target selector for filtering log entries by process or composable logger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceTarget {
    /// Matches all entries from this process.
    Process(ProcessId),
    /// Matches entries from this specific logger under this process.
    Logger(ProcessId, LoggerId),
}

/// Representation of a row in the source hierarchy UI sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    /// Filter target represented by this row (`None` for `All`).
    pub target: Option<SourceTarget>,
    /// Display label for the row.
    pub label: String,
    /// Indentation depth (0 for root/process, 1 for child logger).
    pub depth: u8,
    /// Current lifecycle state of the process/logger.
    pub state: ProcessState,
    /// Historical count of warnings (monotonic across buffer evictions).
    pub warn_count: u64,
    /// Historical count of errors/fatal logs (monotonic across buffer evictions).
    pub error_count: u64,
}

/// A parsed and attributed log entry stored in the ring buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Monotonic sequence number for stable anchoring across evictions.
    pub seq: u64,
    /// Attributed process ID.
    pub process: ProcessId,
    /// Optional attributed composable logger ID.
    pub logger: Option<LoggerId>,
    /// Log severity level.
    pub severity: Severity,
    /// Original raw line with ANSI escape sequences preserved.
    pub raw: Box<str>,
    /// ANSI-stripped plain text used for searching and filtering.
    pub plain: Box<str>,
    /// Byte offset in `raw` where the process prefix ends.
    pub display_offset: usize,
}

impl LogEntry {
    /// Returns the plain text view of the entry without ANSI escapes.
    #[must_use]
    pub fn plain_text(&self) -> &str {
        &self.plain
    }

    /// Returns the raw line view of the entry with original ANSI escapes.
    #[must_use]
    pub fn raw_text(&self) -> &str {
        &self.raw
    }
}

/// Active filtering criteria applied to the log store.
#[derive(Debug, Clone, Default)]
pub struct FilterState {
    selected_sources: HashSet<SourceTarget>,
    min_severity: Option<Severity>,
    query_text: String,
    regex_error: Option<String>,
    compiled_regex: Option<Regex>,
}

impl FilterState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn solo(&mut self, target: Option<SourceTarget>) {
        self.selected_sources.clear();
        if let Some(t) = target {
            self.selected_sources.insert(t);
        }
    }

    pub fn toggle(&mut self, target: Option<SourceTarget>) {
        match target {
            None => self.selected_sources.clear(),
            Some(t) => {
                if !self.selected_sources.remove(&t) {
                    self.selected_sources.insert(t);
                }
            }
        }
    }

    pub fn clear_sources(&mut self) {
        self.selected_sources.clear();
    }

    #[must_use]
    pub fn selected_sources(&self) -> &HashSet<SourceTarget> {
        &self.selected_sources
    }

    pub fn set_min_severity(&mut self, min: Option<Severity>) {
        self.min_severity = min;
    }

    #[must_use]
    pub fn min_severity(&self) -> Option<Severity> {
        self.min_severity
    }

    pub fn set_regex_query(&mut self, query: &str) -> Result<(), String> {
        self.query_text = query.to_string();
        if query.is_empty() {
            self.compiled_regex = None;
            self.regex_error = None;
            return Ok(());
        }

        let pattern = if query.chars().any(|c| c.is_ascii_uppercase()) {
            query.to_string()
        } else {
            format!("(?i){query}")
        };

        match Regex::new(&pattern) {
            Ok(re) => {
                self.compiled_regex = Some(re);
                self.regex_error = None;
                Ok(())
            }
            Err(err) => {
                let err_string = err.to_string();
                self.regex_error = Some(err_string.clone());
                Err(err_string)
            }
        }
    }

    pub fn clear_regex(&mut self) {
        self.query_text.clear();
        self.compiled_regex = None;
        self.regex_error = None;
    }

    #[must_use]
    pub fn query_text(&self) -> &str {
        &self.query_text
    }

    #[must_use]
    pub fn regex_error(&self) -> Option<&str> {
        self.regex_error.as_deref()
    }

    #[must_use]
    pub fn matches(&self, entry: &LogEntry) -> bool {
        if !self.selected_sources.is_empty() {
            let mut matched = self.selected_sources.contains(&SourceTarget::Process(entry.process));
            if !matched
                && let Some(logger) = entry.logger {
                    matched = self
                        .selected_sources
                        .contains(&SourceTarget::Logger(entry.process, logger));
                }
            if !matched {
                return false;
            }
        }

        if let Some(min) = self.min_severity
            && (entry.severity == Severity::Unknown || entry.severity < min) {
                return false;
            }

        if let Some(ref re) = self.compiled_regex
            && !re.is_match(&entry.plain) {
                return false;
            }

        true
    }

    #[must_use]
    pub fn shows_multiple_processes(&self, store: &LogStore) -> bool {
        if self.selected_sources.is_empty() {
            store.process_count() > 1
        } else {
            let mut procs = HashSet::new();
            for target in &self.selected_sources {
                match target {
                    SourceTarget::Process(pid) => {
                        procs.insert(*pid);
                    }
                    SourceTarget::Logger(pid, _) => {
                        procs.insert(*pid);
                    }
                }
            }
            procs.len() >= 2
        }
    }
}

#[derive(Debug, Default)]
struct LoggerCounts {
    warn_count: u64,
    error_count: u64,
}

#[derive(Debug)]
struct ProcessMeta {
    name: String,
    state: ProcessState,
    loggers: Vec<LoggerId>,
    logger_set: HashSet<LoggerId>,
    logger_counts: HashMap<LoggerId, LoggerCounts>,
    warn_count: u64,
    error_count: u64,
}

#[derive(Debug)]
pub struct LogStore {
    max_lines: usize,
    entries: VecDeque<LogEntry>,
    evicted_count: u64,
    next_seq: u64,
    processes: Vec<ProcessMeta>,
    process_map: HashMap<String, ProcessId>,
    logger_names: Vec<String>,
    logger_map: HashMap<String, LoggerId>,
    last_context: HashMap<ProcessId, (Option<LoggerId>, Severity)>,
    last_line: Option<(ProcessId, Option<LoggerId>, Severity)>,
    total_warn_count: u64,
    total_error_count: u64,
}

impl LogStore {
    #[must_use]
    pub fn new(max_lines: usize) -> Self {
        Self {
            max_lines: max_lines.max(1),
            entries: VecDeque::new(),
            evicted_count: 0,
            next_seq: 0,
            processes: Vec::new(),
            process_map: HashMap::new(),
            logger_names: Vec::new(),
            logger_map: HashMap::new(),
            last_context: HashMap::new(),
            last_line: None,
            total_warn_count: 0,
            total_error_count: 0,
        }
    }

    fn get_or_intern_process(&mut self, name: &str) -> ProcessId {
        if let Some(&id) = self.process_map.get(name) {
            return id;
        }
        let id_val = u16::try_from(self.processes.len()).unwrap_or(u16::MAX);
        let id = ProcessId(id_val);
        self.processes.push(ProcessMeta {
            name: name.to_string(),
            state: ProcessState::Passive,
            loggers: Vec::new(),
            logger_set: HashSet::new(),
            logger_counts: HashMap::new(),
            warn_count: 0,
            error_count: 0,
        });
        self.process_map.insert(name.to_string(), id);
        id
    }

    fn get_or_intern_logger(&mut self, name: &str, proc_id: ProcessId) -> LoggerId {
        let lid = if let Some(&id) = self.logger_map.get(name) {
            id
        } else {
            let id_val = u16::try_from(self.logger_names.len()).unwrap_or(u16::MAX);
            let id = LoggerId(id_val);
            self.logger_names.push(name.to_string());
            self.logger_map.insert(name.to_string(), id);
            id
        };

        if let Some(proc_meta) = self.processes.get_mut(proc_id.0 as usize)
            && proc_meta.logger_set.insert(lid)
        {
            proc_meta.loggers.push(lid);
        }
        lid
    }

    pub fn ingest_line(&mut self, raw: &str) -> u64 {
        let parsed = parse_line(raw);
        let (proc_id, logger_id, severity, display_offset) = match parsed.kind {
            LineKind::Launch {
                process,
                severity,
                lifecycle,
            } => {
                let pid = self.get_or_intern_process(&process);
                if let Some(evt) = lifecycle {
                    match evt {
                        LifecycleEvent::Started { pid: child_pid } => {
                            if let Some(proc_meta) = self.processes.get_mut(pid.0 as usize) {
                                proc_meta.state = ProcessState::Running { pid: child_pid };
                            }
                        }
                        LifecycleEvent::Exited { code } => {
                            if let Some(proc_meta) = self.processes.get_mut(pid.0 as usize) {
                                proc_meta.state = ProcessState::Exited { code };
                            }
                        }
                    }
                }
                self.last_line = Some((pid, None, severity));
                (pid, None, severity, 0)
            }
            LineKind::Prefixed {
                process,
                logger,
                severity,
                display_offset,
                is_continuation,
            } => {
                let pid = self.get_or_intern_process(&process);
                if let Some(proc_meta) = self.processes.get_mut(pid.0 as usize)
                    && proc_meta.state == ProcessState::Passive
                {
                    proc_meta.state = ProcessState::Running { pid: None };
                }

                let (lid, sev) = if is_continuation {
                    self.last_context
                        .get(&pid)
                        .copied()
                        .unwrap_or((None, Severity::Unknown))
                } else if let Some(logger_name) = logger {
                    let lid = self.get_or_intern_logger(&logger_name, pid);
                    self.last_context.insert(pid, (Some(lid), severity));
                    (Some(lid), severity)
                } else {
                    // Standalone raw output: resets continuation context
                    self.last_context.remove(&pid);
                    (None, Severity::Unknown)
                };

                if is_continuation || lid.is_some() {
                    self.last_line = Some((pid, lid, sev));
                } else {
                    self.last_line = None;
                }
                (pid, lid, sev, display_offset)
            }
            LineKind::Unprefixed {
                logger,
                severity,
                is_continuation,
            } => {
                let (pid, lid, sev) = if is_continuation {
                    if let Some((prev_pid, prev_lid, prev_sev)) = self.last_line {
                        (prev_pid, prev_lid, prev_sev)
                    } else {
                        let pid = self.get_or_intern_process("(stdout)");
                        (pid, None, Severity::Unknown)
                    }
                } else if let Some(logger_name) = logger {
                    let pid = self.get_or_intern_process("(stdout)");
                    let lid = self.get_or_intern_logger(&logger_name, pid);
                    (pid, Some(lid), severity)
                } else {
                    let pid = self.get_or_intern_process("(stdout)");
                    (pid, None, Severity::Unknown)
                };

                if is_continuation || lid.is_some() {
                    self.last_line = Some((pid, lid, sev));
                } else {
                    self.last_line = None;
                }
                (pid, lid, sev, 0)
            }
        };

        let is_warn = severity == Severity::Warn;
        let is_error = severity == Severity::Error || severity == Severity::Fatal;

        if is_warn {
            self.total_warn_count += 1;
        }
        if is_error {
            self.total_error_count += 1;
        }

        if let Some(proc_meta) = self.processes.get_mut(proc_id.0 as usize) {
            if is_warn {
                proc_meta.warn_count += 1;
            }
            if is_error {
                proc_meta.error_count += 1;
            }
            if let Some(lid) = logger_id {
                let counts = proc_meta.logger_counts.entry(lid).or_default();
                if is_warn {
                    counts.warn_count += 1;
                }
                if is_error {
                    counts.error_count += 1;
                }
            }
        }

        let seq = self.next_seq;
        self.next_seq += 1;

        let entry = LogEntry {
            seq,
            process: proc_id,
            logger: logger_id,
            severity,
            raw: raw.to_string().into_boxed_str(),
            plain: parsed.plain.into_boxed_str(),
            display_offset,
        };

        self.entries.push_back(entry);
        if self.entries.len() > self.max_lines {
            self.entries.pop_front();
            self.evicted_count += 1;
        }

        seq
    }

    #[must_use]
    pub fn entries(&self) -> &VecDeque<LogEntry> {
        &self.entries
    }

    #[must_use]
    pub fn evicted_count(&self) -> u64 {
        self.evicted_count
    }

    #[must_use]
    pub fn process_name(&self, id: ProcessId) -> &str {
        self.processes
            .get(id.0 as usize)
            .map_or("", |p| p.name.as_str())
    }

    #[must_use]
    pub fn logger_name(&self, id: LoggerId) -> &str {
        self.logger_names
            .get(id.0 as usize)
            .map_or("", String::as_str)
    }

    #[must_use]
    pub fn process_state(&self, id: ProcessId) -> ProcessState {
        self.processes
            .get(id.0 as usize)
            .map_or(ProcessState::Passive, |p| p.state)
    }

    #[must_use]
    pub fn process_id(&self, name: &str) -> Option<ProcessId> {
        self.process_map.get(name).copied()
    }

    #[must_use]
    pub fn logger_id(&self, name: &str) -> Option<LoggerId> {
        self.logger_map.get(name).copied()
    }

    #[must_use]
    pub fn source_rows(&self) -> Vec<SourceRow> {
        let mut rows = Vec::new();

        rows.push(SourceRow {
            target: None,
            label: "All".to_string(),
            depth: 0,
            state: ProcessState::Passive,
            warn_count: self.total_warn_count,
            error_count: self.total_error_count,
        });

        for (idx, proc_meta) in self.processes.iter().enumerate() {
            let p_id = ProcessId(u16::try_from(idx).unwrap_or(u16::MAX));
            rows.push(SourceRow {
                target: Some(SourceTarget::Process(p_id)),
                label: proc_meta.name.clone(),
                depth: 0,
                state: proc_meta.state,
                warn_count: proc_meta.warn_count,
                error_count: proc_meta.error_count,
            });

            if proc_meta.loggers.len() >= 2 {
                for &lid in &proc_meta.loggers {
                    let (w_count, e_count) = proc_meta
                        .logger_counts
                        .get(&lid)
                        .map_or((0, 0), |c| (c.warn_count, c.error_count));
                    let label = self
                        .logger_names
                        .get(lid.0 as usize)
                        .map_or("", String::as_str);
                    rows.push(SourceRow {
                        target: Some(SourceTarget::Logger(p_id, lid)),
                        label: label.to_string(),
                        depth: 1,
                        state: proc_meta.state,
                        warn_count: w_count,
                        error_count: e_count,
                    });
                }
            }
        }

        rows
    }

    #[must_use]
    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    #[must_use]
    pub fn filtered_indices(&self, filter: &FilterState) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                if filter.matches(entry) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_composable_logger_tree_expansion_at_two_loggers() {
        let mut store = LogStore::new(100);
        store.ingest_line("[component_container_mt-1] [INFO] [1.0] [planner_server]: ready");

        let rows = store.source_rows();
        assert_eq!(rows.len(), 2, "All + 1 process (single logger not expanded)");
        assert_eq!(rows[0].label, "All");
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[0].target, None);
        assert_eq!(rows[1].label, "component_container_mt-1");
        assert_eq!(rows[1].depth, 0);

        let proc_id = match rows[1].target {
            Some(SourceTarget::Process(pid)) => pid,
            _ => panic!("expected Process target"),
        };

        // Ingest 2nd distinct logger under component_container_mt-1
        store.ingest_line("[component_container_mt-1] [INFO] [2.0] [controller_server]: active");

        let rows2 = store.source_rows();
        assert_eq!(rows2.len(), 4, "All + 1 process + 2 child loggers");
        assert_eq!(rows2[0].label, "All");
        assert_eq!(rows2[0].depth, 0);
        assert_eq!(rows2[1].label, "component_container_mt-1");
        assert_eq!(rows2[1].depth, 0);
        assert_eq!(rows2[1].target, Some(SourceTarget::Process(proc_id)));

        assert_eq!(rows2[2].label, "planner_server");
        assert_eq!(rows2[2].depth, 1);
        let planner_lid = match rows2[2].target {
            Some(SourceTarget::Logger(pid, lid)) => {
                assert_eq!(pid, proc_id);
                lid
            }
            _ => panic!("expected Logger target"),
        };

        assert_eq!(rows2[3].label, "controller_server");
        assert_eq!(rows2[3].depth, 1);
        let controller_lid = match rows2[3].target {
            Some(SourceTarget::Logger(pid, lid)) => {
                assert_eq!(pid, proc_id);
                lid
            }
            _ => panic!("expected Logger target"),
        };
        assert_ne!(planner_lid, controller_lid);
    }

    #[test]
    fn test_continuation_attribution_and_reset() {
        let mut store = LogStore::new(100);
        let seq0 = store.ingest_line("[component_container_mt-1] [ERROR] [1.0] [planner_server]: boom");
        let seq1 = store.ingest_line("[component_container_mt-1]   File \"x.py\", line 42");
        let seq2 = store.ingest_line("[component_container_mt-1] plain stdout");
        let seq3 = store.ingest_line("[component_container_mt-1]   indented after raw");

        assert_eq!(seq0, 0);
        assert_eq!(seq1, 1);
        assert_eq!(seq2, 2);
        assert_eq!(seq3, 3);

        let entries = store.entries();
        assert_eq!(entries.len(), 4);

        // Entry 1 inherits (planner_server, Error)
        assert_eq!(entries[1].severity, Severity::Error);
        assert_eq!(entries[1].logger, entries[0].logger);
        assert!(entries[1].logger.is_some());

        // Entry 2 is standalone raw -> Unknown, logger None
        assert_eq!(entries[2].severity, Severity::Unknown);
        assert_eq!(entries[2].logger, None);

        // Entry 3 is continuation after raw -> resets continuation context, (None, Unknown)
        assert_eq!(entries[3].severity, Severity::Unknown);
        assert_eq!(entries[3].logger, None);

        // Filtering by Logger at Error includes entries 0 and 1, excludes 2 and 3
        let proc_id = entries[0].process;
        let planner_lid = entries[0].logger.unwrap();
        let mut filter = FilterState::new();
        filter.solo(Some(SourceTarget::Logger(proc_id, planner_lid)));
        filter.set_min_severity(Some(Severity::Error));

        let filtered = store.filtered_indices(&filter);
        assert_eq!(filtered, vec![0, 1]);
    }

    #[test]
    fn test_unprefixed_continuation_attribution() {
        let mut store = LogStore::new(100);
        store.ingest_line("[rviz2-3] [ERROR] [1.0] [rviz]: crash");
        store.ingest_line("  Traceback (most recent call last):");
        store.ingest_line("hello plain raw");
        store.ingest_line("  indented line after plain raw");

        let entries = store.entries();
        assert_eq!(entries.len(), 4);

        // Entry 1 inherits rviz2-3, Some(rviz), Error from preceding line
        assert_eq!(entries[1].process, entries[0].process);
        assert_eq!(entries[1].logger, entries[0].logger);
        assert_eq!(entries[1].severity, Severity::Error);
        assert_eq!(entries[1].display_offset, 0);

        // Entry 2 is unprefixed standalone raw -> (stdout), None, Unknown
        assert_eq!(store.process_name(entries[2].process), "(stdout)");
        assert_eq!(entries[2].logger, None);
        assert_eq!(entries[2].severity, Severity::Unknown);

        // Entry 3 is unprefixed continuation after standalone raw -> (stdout), None, Unknown
        assert_eq!(store.process_name(entries[3].process), "(stdout)");
        assert_eq!(entries[3].logger, None);
        assert_eq!(entries[3].severity, Severity::Unknown);
    }

    #[test]
    fn test_ring_buffer_eviction_preserves_monotonic_counts_and_exit_state() {
        let mut store = LogStore::new(3);
        store.ingest_line("[node-1] [WARN] [1.0] [node]: warning 1");
        store.ingest_line("[node-1] [ERROR] [2.0] [node]: error 1");
        store.ingest_line("[INFO] [node-1]: process has died [pid 1234, exit code -9, ...]");
        store.ingest_line("[node-2] [INFO] [3.0] [node]: info 1");
        store.ingest_line("[node-2] [INFO] [4.0] [node]: info 2");

        assert_eq!(store.entries().len(), 3);
        assert_eq!(store.evicted_count(), 2);

        let rows = store.source_rows();
        // Row 0 is All
        assert_eq!(rows[0].label, "All");
        assert_eq!(rows[0].warn_count, 1);
        assert_eq!(rows[0].error_count, 1);

        // Find node-1 row
        let node1_row = rows
            .iter()
            .find(|r| r.label == "node-1")
            .expect("node-1 row exists");
        assert_eq!(node1_row.warn_count, 1);
        assert_eq!(node1_row.error_count, 1);
        assert_eq!(node1_row.state, ProcessState::Exited { code: Some(-9) });
    }

    #[test]
    fn test_filter_state_sources_severities_and_smart_case_regex() {
        let mut store = LogStore::new(100);
        store.ingest_line("[rviz2-1] [INFO] [1.0] [rviz]: Timeout occurred");
        store.ingest_line("[component_container_mt-1] [WARN] [2.0] [nav]: timeout retry");
        store.ingest_line("[component_container_mt-1] [ERROR] [3.0] [planner]: fatal timeout");
        store.ingest_line("[rviz2-1] raw text without severity");

        let entries = store.entries();
        let rviz_pid = entries[0].process;
        let container_pid = entries[1].process;
        let nav_lid = entries[1].logger.unwrap();

        let mut filter = FilterState::new();

        // 1. Source selection: union of Process(rviz) + Logger(container, nav)
        filter.toggle(Some(SourceTarget::Process(rviz_pid)));
        filter.toggle(Some(SourceTarget::Logger(container_pid, nav_lid)));

        assert!(filter.matches(&entries[0])); // rviz
        assert!(filter.matches(&entries[1])); // container, nav
        assert!(!filter.matches(&entries[2])); // container, planner (not in selection)
        assert!(filter.matches(&entries[3])); // rviz raw

        // 2. Severity filter: min_severity = Some(Warn) excludes Info and Unknown
        filter.clear_sources();
        filter.set_min_severity(Some(Severity::Warn));
        assert!(!filter.matches(&entries[0])); // Info < Warn
        assert!(filter.matches(&entries[1])); // Warn == Warn
        assert!(filter.matches(&entries[2])); // Error > Warn
        assert!(!filter.matches(&entries[3])); // Unknown excluded when min_severity is Some

        // 3. Smart-case regex
        filter.set_min_severity(None);

        // "timeout" (all lowercase) -> case-insensitive
        assert!(filter.set_regex_query("timeout").is_ok());
        assert!(filter.matches(&entries[0])); // "Timeout occurred" matches
        assert!(filter.matches(&entries[1])); // "timeout retry" matches

        // "Timeout" (has uppercase) -> case-sensitive
        assert!(filter.set_regex_query("Timeout").is_ok());
        assert!(filter.matches(&entries[0])); // "Timeout occurred" matches
        assert!(!filter.matches(&entries[1])); // "timeout retry" rejected

        // 4. Invalid live regex: returns Err, updates query & error, keeps previous valid regex
        let res = filter.set_regex_query("[unclosed");
        assert!(res.is_err());
        assert_eq!(filter.query_text(), "[unclosed");
        assert!(filter.regex_error().is_some());
        // Previous valid regex ("Timeout") remains active!
        assert!(filter.matches(&entries[0]));
        assert!(!filter.matches(&entries[1]));

        // Clear regex restores all matches
        filter.clear_regex();
        assert_eq!(filter.query_text(), "");
        assert_eq!(filter.regex_error(), None);
        assert!(filter.matches(&entries[0]));
        assert!(filter.matches(&entries[1]));
    }

    #[test]
    fn test_filter_state_shows_multiple_processes() {
        let mut store = LogStore::new(100);
        let mut filter = FilterState::new();

        // 0 processes
        assert!(!filter.shows_multiple_processes(&store));

        store.ingest_line("[proc-1] [INFO] [1.0] [log]: hi");
        // 1 process, selection empty -> false
        assert!(!filter.shows_multiple_processes(&store));

        store.ingest_line("[proc-2] [INFO] [1.0] [log]: hi");
        // 2 processes, selection empty -> true
        assert!(filter.shows_multiple_processes(&store));

        let p1 = store.entries()[0].process;
        let p2 = store.entries()[1].process;

        // Solo 1 process -> false
        filter.solo(Some(SourceTarget::Process(p1)));
        assert!(!filter.shows_multiple_processes(&store));

        // Multi-select 2 targets in same process -> false
        let l1 = store.entries()[0].logger.unwrap();
        filter.clear_sources();
        filter.toggle(Some(SourceTarget::Process(p1)));
        filter.toggle(Some(SourceTarget::Logger(p1, l1)));
        assert!(!filter.shows_multiple_processes(&store));

        // Multi-select across 2 distinct processes -> true
        filter.toggle(Some(SourceTarget::Process(p2)));
        assert!(filter.shows_multiple_processes(&store));
    }

    #[test]
    fn test_filter_state_solo_toggle_clear() {
        let mut filter = FilterState::new();
        let t1 = SourceTarget::Process(ProcessId(1));
        let t2 = SourceTarget::Process(ProcessId(2));

        filter.solo(Some(t1));
        assert_eq!(filter.selected_sources().len(), 1);
        assert!(filter.selected_sources().contains(&t1));

        filter.toggle(Some(t2));
        assert_eq!(filter.selected_sources().len(), 2);

        filter.toggle(Some(t1));
        assert_eq!(filter.selected_sources().len(), 1);
        assert!(filter.selected_sources().contains(&t2));

        // toggle(None) clears
        filter.toggle(None);
        assert!(filter.selected_sources().is_empty());

        filter.solo(Some(t1));
        assert_eq!(filter.selected_sources().len(), 1);
        // solo(None) clears
        filter.solo(None);
        assert!(filter.selected_sources().is_empty());
    }

    #[test]
    fn test_process_lifecycle_transitions() {
        let mut store = LogStore::new(100);

        // 1. Prefixed line for passive process transitions it to Running { pid: None }
        store.ingest_line("[node-1] [INFO] [1.0] [node]: started without launch start line");
        let rows = store.source_rows();
        assert_eq!(rows[1].state, ProcessState::Running { pid: None });

        // 2. Launch start line sets Running { pid: Some(42) }
        store.ingest_line("[INFO] [node-2]: process started with pid [42]");
        let rows = store.source_rows();
        let node2_row = rows.iter().find(|r| r.label == "node-2").unwrap();
        assert_eq!(node2_row.state, ProcessState::Running { pid: Some(42) });

        // 3. Clean exit sets Exited { code: Some(0) }
        store.ingest_line("[INFO] [node-2]: process has finished cleanly");
        let rows = store.source_rows();
        let node2_row = rows.iter().find(|r| r.label == "node-2").unwrap();
        assert_eq!(node2_row.state, ProcessState::Exited { code: Some(0) });
    }
}
