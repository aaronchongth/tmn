# `tmn` Developer Guide

## Architecture & Design Spec
- The canonical specification is in [docs/design.md](docs/design.md).
- Implementation plan: [docs/superpowers/plans/2026-11-06-tmn-v1.md](docs/superpowers/plans/2026-11-06-tmn-v1.md).

## Verification & Commands
Before submitting or completing any task, run:
```bash
cargo test && cargo clippy --all-targets
```
All tests must pass and clippy must report zero warnings.

## Engineering Guidelines & Rules
1. **Zero unwrap in production code:** `Cargo.toml` enforces `unwrap_used = "deny"`. Unit and integration tests may use `unwrap()` and `expect()` as configured in `clippy.toml`.
2. **Standard CLI Contract:**
   - Follow `cli_design_principles.md`.
   - `tmn -h` / `tmn --help` and `tmn -V` / `tmn --version` exit `0`. Help fits within 80 columns.
   - Missing required command / zero args exits `2` with concise usage and examples pointing to `--help`.
   - Invalid flags or `--max-lines 0` exit `2` with an actionable message.
   - Clean quit (`q`) exits `0`.
   - PTY/spawn/internal errors exit `1`.
3. **PTY & Non-Intrusive Capture:**
   - Zero ROS runtime/build dependencies (`rcl`, `rmw`, `rclpy`, `ros2cli` are forbidden).
   - All capture is driven through Unix PTY (`nix::pty::openpty`) and ANSI escape parsing.
