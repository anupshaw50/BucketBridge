# Contributing to BucketBridge

Thanks for helping. BucketBridge is a Tauri 2 desktop app with a React UI and a
CLI (`bb`), all on one Rust core. `docs/development.md` has the full per-OS
setup, and `AGENTS.md` has the project rules and the review checklist (for
people and AI coding agents alike).

## Build and run

Prerequisites: Rust stable (via rustup, with `~/.cargo/bin` on `PATH`), Node.js
and pnpm, and on Linux the Tauri system packages.

```bash
pnpm install
pnpm tauri dev                          # desktop app with hot reload (port 5173)
pnpm --dir ui dev                       # UI alone with the built-in fake backend
cargo build -p bucketbridge-cli && ./target/debug/bb --help
BB_CONFIG_DIR=/tmp/bb-try pnpm tauri dev   # keep your real settings untouched
```

## Test and check

Run these before you open a pull request:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                  # no Docker needed
pnpm --dir ui typecheck && pnpm --dir ui lint && pnpm --dir ui test && pnpm --dir ui build
pnpm --dir ui e2e                       # Playwright smoke tests
cargo deny check && cargo about generate about.hbs
```

Tests that need MinIO: `BB_TEST_BACKEND=minio cargo test -p bucketbridge-tests --features minio`.
Tests never touch the real OS keychain. See `docs/testing.md`.

## Conventions

- When a desktop command or event changes, update `docs/desktop-api.md` first,
  then `ui/src/api` and the fake backend in `ui/src/mock`.
- Every `aws-sdk-*` dependency uses `default-features = false` with
  `default-https-client` and `rt-tokio`.
- Dependencies must be permissively licensed and GPL-3.0 compatible;
  `cargo deny check` must pass.
- Do not use the em dash character in code, docs or UI text.
- Do not copy code, assets or strings from other products.
- User-visible text goes through `tr()` (UI) or `tr`/`trf` (CLI) with a
  Chinese entry; English stays the default. See `docs/i18n.md`.

## Commits

Use conventional commits, for example `feat: add bandwidth limit to sync`,
`fix: keep resume state after a cancelled upload`, `docs: update the roadmap`.
Keep each commit focused.

## Licence

By contributing you agree that your work is licensed under GPL-3.0-only, the
licence of this project.

## Questions and ideas

Use [Discussions](https://github.com/anupshaw50/BucketBridge/discussions) for
questions and early ideas, and issues for bugs and concrete feature requests.
Report security problems privately (see `SECURITY.md`).
