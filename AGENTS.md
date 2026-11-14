# `tmn` Developer Guide

## Architecture & Design Spec
- The canonical specification is in [docs/design.md](docs/design.md).

## Verification & Commands
Before submitting or completing any task, run:
```bash
cargo fmt --check && cargo test && cargo clippy --all-targets -- -D warnings
```
All tests must pass and clippy must report zero warnings. This is exactly what CI
(`.github/workflows/ci.yml`) enforces on every PR.

## Releasing (manual)
1. Bump `version` in `Cargo.toml` (commit the updated `Cargo.lock`) and merge to `main`.
2. Wait for CI to pass on that commit. Its `package` job uploads an artifact named
   `tmn-x86_64-unknown-linux-gnu` (Actions run page -> Artifacts). Download and unzip it to get
   `tmn-x86_64-unknown-linux-gnu.tar.gz` and its `.sha256`.
3. Tag the commit (`git tag vX.Y.Z && git push origin vX.Y.Z`) and create a GitHub release for
   the tag, attaching both files. The README install instructions depend on those exact names.

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
