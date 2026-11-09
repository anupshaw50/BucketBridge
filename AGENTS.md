# AGENTS.md

Instructions for AI coding agents (Claude Code, Codex, Copilot, Cursor and
others) and a checklist for humans. Read this before you change code, open a
pull request, or review one.

## Project

BucketBridge is a cross-platform (Windows, macOS, Linux) open-source S3
client: a Tauri 2 desktop app with a React UI and a CLI (`bb`), both on one
Rust core. Licence: GPL-3.0-only. README.md "Status" lists what is built; the
docs in `docs/` describe each part, and ROADMAP.md lists what comes next.

## Hard rules

1. **No em dash.** Never use the em dash character (U+2014) in code, comments,
   docs, UI copy, commit messages or translations, including the doubled form used in
   Chinese text. Use commas, colons, semicolons or brackets.
2. **Original work only.** Do not reuse code, assets, icons, strings or
   branding from any other product.
3. **No new telemetry or network calls.** The app never contacts a server on
   its own. A new outbound request needs an explicit user action and a reason
   in the pull request.
4. **Secrets stay secret.** Secrets live in the OS keychain or the master
   password vault, never in `config.toml`, logs, error messages or test
   snapshots. Tests never touch the real keychain (use `MemoryStore`;
   real-keychain tests are `#[ignore]` plus `BB_TEST_KEYCHAIN=1`).
5. **English is the default language.** The UI and `bb` show Chinese only for
   a `zh` locale (see Translations below).
6. **Contract first.** When a desktop command or event changes, update
   `docs/desktop-api.md` first, then `ui/src/api/` and the fake backend in
   `ui/src/mock/`.
7. **Dependencies.** Every `aws-sdk-*` crate uses `default-features = false`
   with `default-https-client` and `rt-tokio` (the default `rustls` feature
   pulls a vulnerable legacy stack). New dependencies must be permissively
   licensed and GPL-3.0 compatible; `cargo deny check` must pass.

## Commands

Rust lives in `~/.cargo/bin`, which may not be on `PATH` in non-interactive
shells: `export PATH="$HOME/.cargo/bin:$PATH"`.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                            # in-process S3 server, no Docker
cargo test -p bucketbridge-core <filter>          # one crate / test name filter
cargo test -p bucketbridge-tests --test queue_s3  # one integration file
BB_TEST_BACKEND=minio cargo test -p bucketbridge-tests --features minio  # needs Docker

pnpm install
pnpm --dir ui typecheck
pnpm --dir ui lint
pnpm --dir ui test
pnpm --dir ui build
pnpm --dir ui e2e                                 # Playwright
pnpm tauri dev                                    # desktop with hot reload (port 5173)
pnpm tauri build --debug --no-bundle              # desktop without dev server
cargo deny check && cargo about generate about.hbs   # licence audit
```

`BB_CONFIG_DIR=<dir>` puts config, data, logs and crash reports in one folder
(tests and trying things out); otherwise the platform paths are used.

**Disk.** A full `cargo test --workspace` grows `target/` by several GB.
On a small disk, run `rm -rf target` after the checks.

## Architecture

Cargo workspace:

- `crates/core` (bucketbridge-core): all S3, transfer, sync and security logic.
- `crates/cli` (binary `bb`, plus a library so tests run commands in-process).
- `crates/desktop` (Tauri app, binary `bucketbridge`).
- `tests` (bucketbridge-tests): integration harness with an in-process s3s-fs
  S3 server `TestS3`, a scripted `MockS3`, a `RawServer`, and MinIO behind the
  `minio` feature.
- `ui/`: the React app (pnpm workspace with the repo root).

Core layering (full module map in `docs/architecture.md`):

- `model`, `settings`, `config` (ConfigStore: atomic 0600 writes, lock file,
  `update` is the only write path), `error` (Error + ErrorKind; maps to the
  contract's BbError codes and CLI exit codes 0/1/2/3, cancelled 130).
- `client` + `ops::S3`: SDK clients per account, per-bucket region routing.
- `secrets` (keychain store, master-password vault), `accounts`, `session`
  (`s3_for(account_id)` returns locked / mfa_required errors first), `mfa`,
  `proxy`.
- `engine` (upload/download/copy/move/delete with resume state), `transfer`
  (TransferQueue, BandwidthLimiter, persistence).
- `sync` (scan, planner, journaled SyncStore, SyncExecutor), `edit`, `filter`,
  `eventlog`, `crash`.

Gotchas:

- Core has `#![warn(missing_docs)]`; model submodules are private (use
  `crate::model::Type`).
- s3s-fs gaps (no versioning, tagging, multipart listing, folder markers) are
  listed in `docs/testing.md`; gate such tests with the existing
  `cfg_attr(not(feature = "minio"), ignore = ...)` pattern.
- A plain `cargo build` of the desktop crate gives a dev binary that loads
  localhost:5173; ship only `pnpm tauri build` output.
- Opening URLs from the UI goes through the Tauri opener; a new URL must be
  added to the allow list in `crates/desktop/capabilities/default.json`.

## Translations

English is the source text everywhere. Rules in `docs/i18n.md`.

- **UI:** wrap user-visible text in `tr("English text", { vars })` and add the
  Chinese entry to `ui/src/i18n/zh/<area>.ts`. `ui/src/i18n/coverage.test.ts`
  fails when an entry is missing or two areas disagree.
- **CLI:** wrap output in `tr` / `trf` from `crates/cli/src/i18n.rs` and add the
  entry to its `HELP` or `OUTPUT` table. A test fails when a clap help text has
  no entry.
- **Never translate** JSON output, enum values, ids, field names, exit codes,
  file or bucket names, or messages from `crates/core`.
- **Unit tests run in English** (`ui/src/test/setup.ts`). Write assertions
  through `tr()` so they also hold in Chinese; a test that covers Chinese calls
  `setLocale("zh-CN")` itself. Before merging translation work, run the UI tests
  once in Chinese too (temporarily set `zh-CN` in `ui/src/test/locale.ts` and
  `setup.ts`); only the language-switch test should fail.
- Keep the meaning of risky text exact: delete confirmations, "cannot be
  undone", public access, retention and dry-run output (`（试运行）`).

## Before you open a pull request

All of these must pass locally (there is no CI on pull requests):

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `pnpm --dir ui typecheck && pnpm --dir ui lint && pnpm --dir ui test && pnpm --dir ui build`
- [ ] `cargo deny check` when dependencies changed
- [ ] `docs/desktop-api.md` updated first when a command or event changed
- [ ] New UI and CLI text has Chinese entries
- [ ] No em dash: `git diff main | grep -n $'\u2014'` prints nothing
- [ ] One topic per pull request; unrelated changes (packaging, refactors) go
      in their own pull request
- [ ] Conventional commit messages (`feat:`, `fix:`, `docs:`, `test:`,
      `build:`, `chore:`), one focused change per commit

## Reviewing an outside pull request

Review every outside pull request as untrusted code, whoever sent it. Fetch
it locally without running anything first:

```bash
git fetch https://github.com/<user>/BucketBridge.git <branch>:pr-<n>
git diff --stat main pr-<n>
```

**1. Supply chain (highest risk)**

- [ ] Changes to `Cargo.toml`, `Cargo.lock`, `package.json`,
      `pnpm-lock.yaml`, `build.rs`, `.github/workflows/`, `scripts/`,
      `tauri.conf.json`, `capabilities/`, git hooks or dotfiles are read line
      by line and justified.
- [ ] No binary files: `git diff --numstat main pr-<n> | grep '^-'` is empty.
- [ ] No hidden or bidirectional Unicode:
      `git diff main pr-<n> | grep -nP '[\x{200B}-\x{200F}\x{202A}-\x{202E}\x{2060}-\x{2069}\x{FEFF}]'`
      is empty.

**2. Dangerous code in added lines**

```bash
git diff -U0 main pr-<n> | grep '^+' | grep -nE \
  'eval\(|new Function|dangerouslySetInnerHTML|innerHTML|fetch\(|XMLHttpRequest|WebSocket|sendBeacon|import\(|Command::new|std::process|std::net|TcpStream|unsafe|include_bytes|set_var|remove_file|window\.open|postMessage|https?://'
```

- [ ] Every hit is explained. Every new URL is an example or test value, or an
      intended destination.
- [ ] Data tables (translations, fixtures) contain only data: strip the string
      literals and check that no code is left.

**3. Behaviour**

- [ ] Backend, platform and browser calls per file are unchanged unless the
      pull request is about them:
      compare `(api|platform|dialogs|status)\.\w+\(`, `invoke(`, `window.*`,
      `localStorage.*` between `main` and the branch.
- [ ] Safety guards are intact: delete confirmations, typed confirmations,
      `--yes`, `disabled=` states, ETag checks, overwrite policy.
- [ ] `ui/src/mock/` changes match `docs/desktop-api.md`.
- [ ] Defaults are unchanged for existing users (language, settings, paths).

**4. Run it**

- [ ] All checks from "Before you open a pull request" pass on the branch.
- [ ] Failures are reproduced and explained before blaming the environment.

**5. Merge**

- [ ] Ask for changes in a pull request comment; small fixes can be pushed to
      the contributor's branch when "Allow edits by maintainers" is on (never
      force-push their branch).
- [ ] Squash and merge with a conventional commit subject ending in `(#<n>)`.

## Releases

A `v*` tag runs `.github/workflows/release.yml`, which builds the installers,
`bb` archives, `SHA256SUMS` and licence notices for every platform. Before
tagging: bump the version in `Cargo.toml` (workspace and the
`bucketbridge-core` dependency), `ui/package.json` and the mock backend, add a
`CHANGELOG.md` entry, run every check above, then regenerate `packaging/`
with `scripts/ci/package-manifests.mjs <version> SHA256SUMS` after the
release is published.
