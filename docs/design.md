# `tmn` (`too-many-nodes`) — Design Specification

## 1. Purpose & Non-Goals

`tmn` (*too-many-nodes*) is a minimal, single-binary Linux terminal UI wrapper for `ros2 launch` (and arbitrary multi-process commands). It runs the target command inside a pseudo-terminal, captures the full output firehose in memory, and provides a `k9s`-inspired TUI to filter logs interactively by OS process, inner ROS logger (including composable nodes inside `component_container*`), severity, and regular expression.

### Design Stance
- **Zero intrusion into ROS:** No linkage against `rcl`, `rmw`, `rclpy`, or `ros2cli`; no `/rosout` subscriptions; no `enable_logger_service` requirement. Works identically across ROS 2 Humble, Jazzy, Kilted, and Lyrical, as well as across host shells, Distrobox, Docker, and `pixi`.
- **Captures the real terminal stream:** Unlike `/rosout` tools, `tmn` captures Python `print()` statements, uncaught tracebacks, C/C++ `stdout`/`stderr`, sanitizer/segfault backtraces, Gazebo/RViz output, and `launch` process-lifecycle events.
- **Minimal v1 scope (YAGNI):**
  - **In scope:** Single-terminal PTY wrapper, process + inner-logger discovery and multi-select, minimum-severity filter, live regex filter, auto-follow scrollback with pause-on-scroll, sidebar zoom, and post-mortem hold on `Ctrl-C` / child exit with orphan-free shutdown.
  - **Out of scope for v1:** RMW/service log-level mutation, passive `~/.ros/log` attach mode, in-place command restart (`r`), and clipboard/file export.

---

## 2. CLI Contract

```text
tmn [OPTIONS] [--] <COMMAND> [ARGS]...
```

### Options
| Flag | Default | Behavior |
|------|---------|----------|
| `-n, --max-lines <COUNT>` | `100000` | Maximum log entries retained in the ring buffer (`COUNT >= 1`). |
| `--no-env-override` | `false` | Do not inject default ROS/Python buffering and output env vars into the child. |
| `--headless` | `false` | Run `<COMMAND> [ARGS]...` through the PTY, parser, and store without opening the TUI; on child exit, print captured display lines to `stdout` and a per-source summary to `stderr`. |
| `-h, --help` | — | Print structured help (description, usage, progressive examples, options) within 80 columns and exit `0`. |
| `-V, --version` | — | Print `tmn <version>` and exit `0`. |

### Argument & Exit-Code Rules
- `<COMMAND> [ARGS]...` uses trailing variable arguments (`trailing_var_arg = true`, `allow_hyphen_values = true`) so flags intended for `ros2 launch` (e.g., `tmn ros2 launch demo_nodes_cpp talker_listener.launch.py --ros-args ...`) pass through without requiring `--`, while `tmn -- ...` is also supported explicitly.
- **No arguments (`tmn`):** Writes a concise 1-line description, usage synopsis, two examples, and `Run 'tmn --help' for all options.` to `stderr`, then exits with code `2`.
- **Invalid flags / `--max-lines 0`:** Writes an actionable error message to `stderr` and exits with code `2`.
- **Interactive mode exit codes:** `0` when the user quits cleanly (`q`); `1` if PTY allocation, child spawn, or terminal initialization fails.
- **`--headless` mode exit codes:** Returns the child's exit code (`128 + signal` if killed by a signal); `1` if PTY/spawn fails.

---

## 3. Child Environment & PTY Lifecycle

### Default Child Environment
Unless `--no-env-override` is passed, `tmn` sets the following variables in the child environment **only when unset in the caller's environment** (never clobbering explicit user overrides):

| Variable | Default Value | Reason |
|----------|---------------|--------|
| `OVERRIDE_LAUNCH_PROCESS_OUTPUT` | `both` | Forces `ros2 launch` (`ExecuteLocal`) to emit both `stdout` and `stderr` to screen even for actions using default `output='log'`. |
| `RCUTILS_COLORIZED_OUTPUT` | `1` | Preserves severity colors from `rcutils`. |
| `RCUTILS_LOGGING_BUFFERED_STREAM` | `0` | Disables internal `rcutils` stream buffering for immediate line delivery. |
| `PYTHONUNBUFFERED` | `1` | Forces Python nodes and scripts to flush `print()` output immediately. |

### PTY & Signal Semantics
1. **Session & controlling terminal:** `tmn` opens a Unix PTY pair (`nix::pty::openpty`) sized to the current terminal dimensions (fallback `80x24` when unavailable, e.g., `--headless`). In `pre_exec`, the child calls `setsid()` and `ioctl(slave_fd, TIOCSCTTY, 0)` so the child's PID equals its PGID (`child_pgid = child_pid`) and `isatty(0/1/2)` is true.
2. **Window resize:** Terminal `Resize(cols, rows)` events update the PTY master window size (`TIOCSWINSZ`) to match the log viewport dimensions.
3. **Linux PTY EOF (`EIO`):** When all processes holding the PTY slave FD close it, Linux `read()` on the master FD returns `Errno::EIO` (`os error 5`). The PTY reader treats both `Ok(0)` and `Err(EIO)` as clean stream EOF, flushes any buffered partial line without trailing `\n`, and terminates cleanly.
4. **Non-UTF-8 resilience:** Incoming byte slices are split on `\n` (stripping a single preceding `\r` for PTY `\r\n` line endings) and decoded via `String::from_utf8_lossy` so binary garbage from crashing C++ nodes never panics or drops neighboring lines.
5. **Shutdown state machine (zero orphan processes):**
   - **`Running` → `Ctrl-C` pressed:** Sends `SIGINT` to `-child_pgid` (`killpg`). State transitions to `Stopping`. `ros2 launch` executes its normal graceful shutdown cascade (`SIGINT` → `SIGTERM` → `SIGKILL` to its children) while `tmn` remains open, streaming shutdown logs live. Repeated `Ctrl-C` while `Stopping` sends `SIGINT` on the 2nd press and `SIGTERM` on subsequent presses.
   - **Child reaps (`waitpid`):** State transitions to `Exited(ExitOutcome)` (`Code(i32)` or `Signal(i32)`). In interactive TUI mode, `tmn` stays open in **post-mortem hold** so crash and shutdown logs remain searchable and filterable until the user presses `q`.
   - **`q` pressed while `Exited`:** Restores terminal and exits `0` immediately.
   - **`q` pressed while `Running` or `Stopping`:** Enters `Quitting` — sends `SIGINT` to `-child_pgid` (if still `Running`) and waits up to `5s` before escalating to `SIGTERM` and another `2s` before `SIGKILL`, exiting automatically once `waitpid` reaps the child. Pressing `q` or `Ctrl-C` again while `Quitting` escalates immediately (`SIGTERM`, then `SIGKILL`).
   - **External signals (`SIGTERM` / `SIGHUP`) & panics:** External `SIGTERM`/`SIGHUP` triggers the `Quitting` escalation path; a `Drop` guard (`TerminalGuard`) and panic hook guarantee raw mode and alternate screen are always restored.

---

## 4. Log Parsing & Source Attribution

Every line is stored raw (with original ANSI SGR styling intact for viewport rendering) while classification runs on an ANSI-stripped view (`plain`, stripping CSI `\x1b\[[0-9;?]*[A-Za-z]` and OSC sequences).

### Classification Precedence (evaluated top-to-bottom)

1. **`ros2 launch` own & process-lifecycle lines:**
   Matches optional leading float timestamp `^(?:\d+\.\d+\s+)?\[(?P<sev>DEBUG|INFO|WARNING|WARN|ERROR|FATAL|CRITICAL)\] \[(?P<actor>[^\]]+)\]: (?P<body>.*)$`:
   - If `actor == "launch"`: `process = "launch"`, `logger = None`, `severity = parse_sev(sev)`, `display_offset = 0`.
   - If `actor` ends with `-\d+` (e.g., `rviz2-3`): `process = actor`, `logger = None`, `severity = parse_sev(sev)`, `display_offset = 0`, plus optional `LifecycleEvent`:
     - `body` contains `process started with pid [` → `Started { pid }`
     - `body` starts with `process has finished cleanly` → `Exited { code: Some(0) }`
     - `body` starts with `process has died [` → `Exited { code }` (parsed from `exit code (-?\d+)`, fallback `None`)

2. **`ExecuteLocal` child-prefixed lines:**
   Matches `^(?:\d+\.\d+\s+)?\[(?P<proc>[^\[\]]+-\d+)\] (?P<rest>.*)$` (matching `ros2/launch`'s unconditional `<name>-<counter>` format so bare `[INFO]` headers from non-`launch` commands are never misread as process names):
   - Sets `process = proc`, and computes `display_offset` as the byte index in `raw` immediately after the matched `[<proc>] ` prefix (accounting for any leading ANSI sequences or epoch timestamp, fallback `0`) so solo views omit the redundant `[proc-N] ` prefix cleanly.
   - Inspects `rest` (ANSI-stripped) for an `rcutils` log header:
     `^\[(?P<sev>DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\] (?:\[\d+(?:\.\d+)?\] )?\[(?P<logger>[^\]]+)\]: (?P<msg>.*)$`
     - **Match:** `severity = parse_sev(sev)`, `logger = Some(logger)`. Updates `last_context[process] = (logger, severity)`.
     - **No match, continuation line** (`rest` starts with whitespace, `Traceback (most recent call last):`, `Caused by:`, `File "`, `at `, or `#\d+\s`): inherits `(logger, severity)` from `last_context[process]` if present; otherwise `(None, Unknown)`.
     - **No match, standalone raw output** (e.g., `Gtk-Message: ...` or `print("hello")`): `logger = None`, `severity = Unknown`, clears `last_context[process]`.

3. **Unprefixed lines (direct binary / shell script / raw output):**
   - Checks `plain` directly against the `rcutils` regex above:
     - **Match:** `process = "(stdout)"`, `logger = Some(logger)`, `severity = parse_sev(sev)`.
     - **Continuation line:** inherits `(process, logger, severity)` from the immediately preceding line if present; otherwise `("(stdout)", None, Unknown)`.
     - **Standalone raw line:** `process = "(stdout)"`, `logger = None`, `severity = Unknown`.

---

## 5. Data Model, Ring Buffer & Filtering

### Ring Buffer (`LogStore`)
- Backed by `VecDeque<LogEntry>` bounded to `max_lines`. When full, `pop_front()` evicts the oldest entry (`evicted_count += 1`); per-source historical `warn_count` / `error_count` and process lifecycle state are **monotonic** across evictions so an earlier startup error badge does not silently disappear when spam rolls the buffer.
- `process` and `logger` names are interned into compact `ProcessId(u16)` / `LoggerId(u16)` handles so each `LogEntry` stays small (~48 bytes + single `Box<str>` allocation).
- **Composable-node tree rule:** Every discovered `ProcessId` appears as a top-level source row in discovery order beneath `All`. Child `LoggerId` sub-rows under a process are exposed in the UI list **only when that process has `>= 2` distinct loggers** (e.g., `component_container_mt-1` hosting multiple composable nodes). Single-logger processes stay a single flat row to avoid duplicate visual noise.

### Filter Semantics (`FilterState`)
An entry passes the active filter iff **all three** hold:
1. **Source filter:** Selection set is empty (`All` mode), OR `Selected::Process(entry.process)` is in the set, OR `entry.logger` is `Some(l)` and `Selected::Logger(entry.process, l)` is in the set.
2. **Severity filter:** `min_severity == None` (`ALL`), OR `entry.severity >= min_severity` (`Debug < Info < Warn < Error < Fatal`; `Unknown` lines are shown only when `min_severity == None`).
3. **Regex filter:** Active compiled `regex::Regex` is `None`, OR matches `entry.plain_text()` (case-insensitive by default `(?i)` when pattern contains no uppercase ASCII, smart-case when pattern contains uppercase). While typing in `/` mode, invalid partial regex syntax preserves the last valid compiled regex and highlights the input border in yellow with the error reason instead of blanking the screen or crashing.

---

## 6. Minimal TUI Layout & Keybindings

### Screen Regions
1. **Top status bar (1 line):** Command summary, lifecycle badge (`RUNNING` green / `STOPPING...` yellow / `EXITED (0)` dim / `CRASHED (code)` red), active severity & regex badges, visible/total line counter, and `[PAUSED — press G to follow]` indicator when tail-follow is disengaged.
2. **Main split area:**
   - **Left source pane (~28 cols, collapsible via `z`):** `All` row at top, followed by processes (and indented child loggers for multi-logger processes). Each row shows selection marker (`*` when selected, space otherwise), status dot (`●` running, `✓` exited 0, `✗` crashed/non-zero, `·` passive), name, and compact `!W`/`×E` counters when `> 0`.
   - **Right log viewport (`1fr`):** Renders only the visible slice of matching entries, converting raw ANSI SGR escape sequences lazily via `ansi-to-tui`. When showing multiple processes (`All` or multi-select across `>= 2` processes), prepends a dim `[proc-N]` gutter tag; when soloing a single process or logger, omits the redundant `[proc-N]` prefix to maximize horizontal space.
3. **Bottom bar (1 line):** Concise key hint bar in normal mode; live `/` search input prompt in search mode.

### Keybindings
| Key | Action |
|-----|--------|
| `↑` / `k`, `↓` / `j` | Move cursor in the source list (when sidebar visible) or scroll log lines (`1` line) when sidebar is zoomed (`z`). |
| `Enter` | Solo the highlighted row (`All` clears all source selections; process/logger selects only that item). |
| `Space` | Toggle the highlighted process/logger in the multi-select set (on `All`, clears the selection set). |
| `a` | Clear source selection (`All` sources). |
| `1` / `2` / `3` / `4` / `5` | Set minimum severity: `1`=ALL, `2`=DEBUG+, `3`=INFO+, `4`=WARN+, `5`=ERROR+. |
| `/` | Enter regex search input mode (`Enter` commits filter and returns to normal mode; `Esc` clears regex filter and returns to normal mode; `Ctrl-U` clears input buffer; in normal mode `Esc` also clears an active regex filter). |
| `PgUp` / `PgDn`, `Ctrl-U` / `Ctrl-D` | Scroll log viewport by half/full page; scrolling up disengages tail-follow automatically. |
| `g` / `G` | Jump to oldest buffered line (`g`, pauses follow) / jump to latest line & re-engage tail-follow (`G`). |
| `z` | Toggle sidebar visibility (full-width log zoom). |
| `Ctrl-C` | Stop child process group (`SIGINT`) and hold TUI in post-mortem mode (escalates if pressed repeatedly while stopping). |
| `q` | Quit `tmn` (cleanly stops child first if still running). |

---

## 7. Module Map & Testing Strategy

### Module Map
- `src/main.rs` — Entry point: parses `Cli`, dispatches `--headless` vs interactive TUI, maps errors to exit codes. Zero business logic.
- `src/lib.rs` — Public module exports (`cli`, `pty`, `parser`, `store`, `app`, `ui`).
- `src/cli.rs` — `clap` CLI definition, validation, and 0-arg help text.
- `src/pty.rs` — PTY spawn, env defaults, reader/reaper threads, `PtyEvent` channel, and process-group signal escalation.
- `src/parser.rs` — ANSI stripper, `ros2 launch` / `rcutils` parser, and `LifecycleEvent` extractor.
- `src/store.rs` — Interned `SourceTree`, bounded `LogStore` ring buffer, continuation attribution, and `FilterState`.
- `src/app.rs` — Pure UI & lifecycle state machine (`App`), scroll/follow calculation, and key/PTY event reducer.
- `src/ui.rs` — `ratatui` layout/widgets, lazy ANSI viewport conversion, and `TerminalGuard`.

### Verification Levels (No ROS Installation Required)
1. **Unit tests (`#[cfg(test)]` alongside each module):**
   - `cli`: no-arg exit `2` + stderr copy, `--max-lines 0` rejection, trailing hyphenated `--ros-args` passthrough, `--help`/`--version`.
   - `parser` & `store`: real `ros2 launch` lines (including ANSI colors, composable-node containers, Python/C++ multi-line tracebacks, process start/death lifecycle transitions, non-ROS raw lines, ring-buffer eviction preserving error counts, smart-case & invalid live regex).
   - `app` & `ui`: `ratatui::backend::TestBackend` frame snapshots verifying solo (`Enter`), multi-select (`Space`), composable logger expansion (`>= 2` loggers only), scroll pause/resume (`PgUp` / `G`), sidebar zoom (`z`), and `Ctrl-C` post-mortem state transitions.
2. **Real PTY & signal integration tests (`tests/pty_integration.rs`):**
   - Spawns real Linux child shell/Python scripts through `PtySession`: verifies `isatty() == true`, default env injection + explicit env preservation, Linux `EIO` on slave close, non-UTF-8 byte survival, `Ctrl-C` `SIGINT` forwarding into post-mortem hold, stubborn-child `SIGTERM`/`SIGKILL` escalation on `q` without leaking orphan processes, and `--headless` end-to-end binary execution.
