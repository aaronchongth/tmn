# tmn (too-many-nodes)

A minimal, single-binary Linux terminal UI wrapper for `ros2 launch` (and arbitrary multi-process commands). It runs the target command inside a pseudo-terminal, captures the full output firehose in memory, and provides a `k9s`-inspired TUI to filter logs interactively.

## Usage

```sh
tmn [OPTIONS] [--] <COMMAND> [ARGS]...
```

### Examples

Wrap a `ros2 launch` command:
```sh
tmn ros2 launch demo_nodes_cpp talker_listener.launch.py
```

Run a simple multi-process script:
```sh
tmn sh -c "echo 'hello'; sleep 1; echo 'world'"
```

Headless mode (capture output directly to stdout/stderr):
```sh
tmn --headless ros2 launch demo_nodes_cpp talker_listener.launch.py
```

## Keybindings

| Key | Action |
|-----|--------|
| `↑` / `k`, `↓` / `j` | Move cursor in the source list or scroll log lines |
| `Enter` | Solo the highlighted row (`All` clears all source selections) |
| `Space` | Toggle the highlighted process/logger in the multi-select set |
| `a` | Clear source selection (`All` sources) |
| `1` / `2` / `3` / `4` / `5` | Set minimum severity: `1`=ALL, `2`=DEBUG+, `3`=INFO+, `4`=WARN+, `5`=ERROR+ |
| `/` | Enter regex search input mode |
| `PgUp` / `PgDn`, `Ctrl-U` / `Ctrl-D` | Scroll log viewport |
| `g` / `G` | Jump to oldest buffered line / jump to latest line & re-engage tail-follow |
| `z` | Toggle sidebar visibility |
| `Ctrl-C` | Stop child process group (`SIGINT`) and hold TUI in post-mortem mode |
| `q` | Quit `tmn` (cleanly stops child first if still running) |
