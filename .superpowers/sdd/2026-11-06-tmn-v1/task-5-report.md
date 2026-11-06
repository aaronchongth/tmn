# Task 5 Report: Linux PTY Session, Signal Escalation & End-to-End Wiring

## Overview
Implemented `src/pty.rs` and wired it into `src/main.rs`. We successfully introduced PTY capabilities, including signal forwarding, terminal size propagation, and robust non-UTF-8 stream parsing, as well as end-to-end testing for both headless and interactive modes.

## Implementation Details

1. **PTY Session Management (`src/pty.rs`)**:
   - `openpty` is used to allocate the PTY master and slave pair with specified terminal dimensions.
   - For interactive mode, we clone the slave FD to the `Command`'s stdin, stdout, and stderr.
   - Using `CommandExt::pre_exec`, `setsid()` and `ioctl(TIOCSCTTY)` are called to establish the child as a session leader, ensuring `child_pgid == child_pid`.
   - The parent correctly drops the slave immediately after `Command::spawn()` so when the child finishes, the master receives an `EIO` event marking the EOF of the data stream.
   - PTY reader and reaper run on dedicated threads. We use a wait mechanism (`reader_done` condvar) so that when `child.wait()` yields an exit status, we give up to 100ms for any buffered raw stream to be flushed into `tx.send(PtyEvent::Line(last))` before firing `PtyEvent::Exited`.
2. **Robust Byte Stream Splitting**:
   - `ByteLineSplitter` properly handles binary logs (e.g. from failing C++ nodes). We split byte frames by newline (`\n`), stripping `\r` smoothly.
   - Multi-byte characters split across `read()` boundaries are properly lossy-decoded using `String::from_utf8_lossy`.
3. **End-to-End Wiring (`src/main.rs`)**:
   - **Headless Mode**: Spins up the `LogStore`, captures output from the PTY directly to stdout, and on process exit, reports a per-process summary to stderr, exiting with `outcome.shell_exit_code()`.
   - **Interactive Mode**: Hooks `SIGHUP` and `SIGTERM` with `signal-hook` in a boolean flag, passing shutdown requests safely into the `App::request_external_shutdown()` state machine. Polling checks input via `crossterm` and updates `ratatui` with 16ms render ticks.
4. **Integration Tests (`tests/pty_integration.rs`)**:
   - Spawns simple `sh` / `python3` scripts over PTYs.
   - Tests assert environments, behavior on single final lines (e.g., Segfault without newline), Ctrl-C hold on TUI, rapid escalation logic (SIGINT -> SIGTERM -> SIGKILL), and full headless CLI execution logic with appropriate codes.

## TDD Output

**Red (Cargo test failure before fixes):**
```
error: test failed, to rerun pass `--test pty_integration`

Caused by:
  process didn't exit successfully: `/usr/local/google/home/aaronchong/workspaces/logging/tmn/target/debug/deps/pty_integration-...` (signal: 6, SIGABRT: process abort signal)
```
*(Failed due to IO Safety violation from double dropping an `OwnedFd` inside `Stdio::from_raw_fd(slave)` vs manual `drop(slave)`).*

**Green (Cargo test pass after fixes):**
```
running 5 tests
test quitting_escalates_to_sigkill_when_child_ignores_sigint ... ok
test pty_flushes_unterminated_final_line_on_slave_close_eio ... ok
test pty_sets_tty_and_default_env_without_clobbering_caller ... ok
test headless_binary_runs_end_to_end_and_propagates_exit_code ... ok
test ctrl_c_forwards_sigint_and_holds_post_mortem ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
```
*(0 Clippy warnings overall).*

## Commit
I will now commit the work.
## Fix Round 1
Addressed 5 findings (Spec & Terminal Corruption, PTY Master FD Use-After-Close, `unwrap()` lint violation, Quit Escalation Deadlines / Double Read, and Minor cleanups).
Command run: `cargo test --test pty_integration && cargo test && cargo clippy --all-targets`
Output: 47/47 tests passed (5 in `pty_integration.rs`, 42 in unit tests). 0 failures, 0 ignored. 0 clippy warnings.
