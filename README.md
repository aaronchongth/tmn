# tmn (too-many-nodes)

A minimal, single-binary Linux terminal UI wrapper for `ros2 launch` (and arbitrary multi-process commands). It runs the target command inside a pseudo-terminal (PTY), captures the full output firehose in memory, and provides a `k9s`-inspired TUI to filter logs interactively by OS process, composable ROS logger, severity, and regex—with zero linkage to `rcl`, `rmw`, or Python `ros2cli`.

## Installation

**Prerequisites:** Linux and Rust `>= 1.91` (`cargo` / `rustup`).

Install from a local checkout into `~/.cargo/bin/tmn`:

```sh
cargo install --path . --locked
```

Ensure `~/.cargo/bin` is on your `PATH`. Because `tmn` is a standalone binary with zero ROS dependencies, a single install in `~/.cargo/bin` works across ROS 2 Humble, Jazzy, Kilted, and Lyrical, as well as inside Pixi shells or Distrobox containers that share `$HOME`.

## Usage

```sh
tmn [OPTIONS] [--] <COMMAND> [ARGS]...
```

### Examples

Wrap a `ros2 launch` command (inside a sourced ROS shell, Pixi env, or Distrobox):
```sh
tmn ros2 launch demo_nodes_cpp talker_listener_launch.py
```

Pass trailing `--ros-args` directly (no `--` needed unless `<COMMAND>` starts with `-`):
```sh
tmn ros2 launch demo_nodes_cpp talker_listener_launch.py --ros-args -r __ns:=/bot1
```

Wrap `distrobox enter` from the host:
```sh
tmn distrobox enter ubuntu26 -- bash -lc "source /opt/ros/lyrical/setup.bash && ros2 launch demo_nodes_cpp talker_listener_launch.py"
```

Headless mode (run through PTY & parser without TUI; print logs to `stdout` and source summary to `stderr`):
```sh
tmn --headless ros2 launch demo_nodes_cpp talker_listener_launch.py
```

### Options & Default Environment Overrides

| Option | Default | Description |
|--------|---------|-------------|
| `-n, --max-lines <COUNT>` | `100000` | Maximum log lines retained in the in-memory ring buffer (`COUNT >= 1`). |
| `--no-env-override` | `false` | Do not inject default ROS/Python buffering & output env vars into the child. |
| `--headless` | `false` | Capture output without opening the TUI; print lines to `stdout` and summary to `stderr`. |
| `-h, --help` | — | Print help and examples. |
| `-V, --version` | — | Print version. |

Unless `--no-env-override` is passed, `tmn` sets the following variables in the child environment **only if unset** in your shell (never clobbering explicit overrides):
- `OVERRIDE_LAUNCH_PROCESS_OUTPUT=both` — ensures `ros2 launch` actions with default `output='log'` still emit `stdout`/`stderr` to the PTY.
- `RCUTILS_COLORIZED_OUTPUT=1` — preserves ANSI severity colors.
- `RCUTILS_LOGGING_BUFFERED_STREAM=0` & `PYTHONUNBUFFERED=1` — flushes C++ `rcutils` and Python `print()` lines immediately.

*(Note: `docker exec` strips host env vars unless `-e` is passed; prefer running `tmn` inside the container or passing `-e OVERRIDE_LAUNCH_PROCESS_OUTPUT=both`.)*

## Scrolling, Wrapping & Copying Logs

Because `tmn` runs in the terminal's alternate screen with a two-pane split by default:
1. **Press `z` (Zoom)** to hide the left source panel full-width. While zoomed:
   - Drag-selecting text in your terminal copies **only log lines** without the left sidebar.
   - `j` / `k`, `↑` / `↓`, and terminal mouse-wheel (alternate-scroll) scroll the log viewport directly.
2. **Press `w` (Wrap)** to soft-wrap long lines to the viewport width so nothing at the right edge is truncated on screen or when copying.
3. **While the sidebar is open**, use `Ctrl-U` / `Ctrl-D` (half-page), `PgUp` / `PgDn` (full-page), or `g` / `G` (top / resume live tail) to scroll logs.
4. **Press `?` (Help)** anytime to view all shortcuts in an overlay popup (hold-to-peek on terminals supporting the Kitty keyboard protocol such as Kitty, WezTerm, Ghostty, and Alacritty; press `?` or `Esc` to close on standard VTE/tmux/web terminals).

## Keybindings

| Key | Action |
|-----|--------|
| `↑` / `k`, `↓` / `j` | Move cursor in the source sidebar (or scroll logs `1` line when zoomed via `z`) |
| `Enter` | Solo the highlighted row (`All` clears all source selections) |
| `Space` | Toggle the highlighted process/logger in the multi-select set |
| `a` | Clear source selection (`All` sources) |
| `z` | Toggle sidebar visibility (zoom logs full-width for mouse scroll & clean copy) |
| `w` | Toggle soft line wrapping (`[WRAP]` badge in top bar) |
| `Ctrl-U` / `Ctrl-D` | Scroll log viewport half-page up / down (pauses tail-follow) |
| `PgUp` / `PgDn` | Scroll log viewport full-page up / down |
| `g` / `G` | Jump to oldest buffered line / jump to latest line & resume tail-follow |
| `1` / `2` / `3` / `4` / `5` | Minimum severity: `1`=ALL, `2`=DEBUG+, `3`=INFO+, `4`=WARN+, `5`=ERROR+ |
| `/` , `Esc` | Regex search (case-insensitive unless uppercase is used) / clear active regex |
| `?` | Show shortcuts popup (hold `?` on key-release terminals, or toggle with `?` / `Esc`) |
| `Ctrl-C` | Send `SIGINT` to child process group and hold TUI open in post-mortem mode |
| `q` | Quit `tmn` (cleanly stops child first via `SIGINT` -> `SIGTERM` -> `SIGKILL` escalation) |

## Caveats & Limitations

- **In-memory buffer bounds line count, not byte size:** `--max-lines` (default `100,000`, ~30–50 MB RAM for typical ROS logs) bounds entry count. Commands that emit multi-megabyte single lines or continuous `\r`-only progress bars without `\n` can increase memory usage; lower `-n` if wrapping extreme bulk dumps.
- **`~/.ros/log` disk usage under `OVERRIDE_LAUNCH_PROCESS_OUTPUT=both`:** `tmn` itself never reads or writes files on disk, but `OVERRIDE_LAUNCH_PROCESS_OUTPUT=both` instructs `ros2 launch` to write `output='screen'` node streams to `~/.ros/log/<run_id>/launch.log` as well as the terminal. For multi-hour, high-volume simulations, monitor `~/.ros/log` disk space or run with `OVERRIDE_LAUNCH_PROCESS_OUTPUT=screen tmn ros2 launch ...`.
- **No interactive `stdin` passthrough:** `tmn` captures keyboard input for TUI controls and does not forward keystrokes into the child PTY. Do not wrap commands that block on interactive prompts (`sudo`, `ssh` passphrases, `[y/N]` confirmations, or `pdb`).
