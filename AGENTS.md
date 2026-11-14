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
Build on an Ubuntu 24.04+ host: the binary links against the host glibc, so the README's
"glibc 2.39+ / Ubuntu 24.04 or newer" requirement follows from where it is built. Run from the repo root.
1. Bump `version` in `Cargo.toml`, commit the updated `Cargo.lock`, and merge to `main` once CI passes.
2. Build and package (file names must stay exactly as-is; the README install instructions use them):
   ```bash
   git status --short            # should be clean
   cargo build --release --locked
   mkdir -p dist
   tar -czf dist/tmn-x86_64-unknown-linux-gnu.tar.gz -C target/release tmn -C ../.. LICENSE README.md
   (cd dist && sha256sum tmn-x86_64-unknown-linux-gnu.tar.gz > tmn-x86_64-unknown-linux-gnu.tar.gz.sha256)
   (cd dist && sha256sum -c tmn-x86_64-unknown-linux-gnu.tar.gz.sha256)
   ```
3. Tag, push, then create the GitHub release from the tag attaching both files in `dist/`
   (web UI, or `gh release create vX.Y.Z dist/* --generate-notes`):
   ```bash
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```

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
