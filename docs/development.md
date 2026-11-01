# Development guide

How to build, test, and lint BucketBridge on Windows, macOS, and Linux. The
contract between the desktop host and the UI is `docs/desktop-api.md`.
Releases and the licence audit are covered in `docs/packaging.md`.

## Repository layout

| Path | Contents |
|---|---|
| `crates/core` | `bucketbridge-core`: data model, configuration, S3 transport |
| `crates/cli` | `bucketbridge-cli`: the `bb` command-line tool |
| `crates/desktop` | `bucketbridge-desktop`: the Tauri 2 desktop app |
| `tests` | `bucketbridge-tests`: integration test harness |
| `ui` | React + TypeScript + Vite user interface (pnpm workspace member) |
| `scripts/ci` | Node.js helpers used by the workflows |
| `.github/workflows` | the release workflow |

## Prerequisites

All platforms:

- **Rust**, stable channel, installed with [rustup](https://rustup.rs), plus
  the formatter and linter: `rustup component add rustfmt clippy`.
- **Node.js 26** (the current LTS line, also used by CI).
- **pnpm 12**, for example `npm install --global pnpm@12`. CI uses the version
  pinned by the `packageManager` field of the root `package.json` when that
  field exists, and pnpm 12 otherwise.
- **Docker**, only for the MinIO integration tests.

### Linux (Debian and Ubuntu)

Tauri 2 needs WebKitGTK 4.1 and a few other libraries, and the keyring
crate's Secret Service backend needs the D-Bus headers:

```sh
sudo apt-get update
sudo apt-get install -y build-essential curl wget file pkg-config \
  libwebkit2gtk-4.1-dev libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev
```

For other distributions, install the equivalents listed in the
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) plus the D-Bus
development package.

### macOS

- Xcode Command Line Tools: `xcode-select --install`.
- Only for universal builds: `rustup target add aarch64-apple-darwin x86_64-apple-darwin`.

### Windows

- Microsoft C++ Build Tools with the "Desktop development with C++" workload.
- The WebView2 runtime, which Windows 10 and 11 already include.
- NASM for the `aws-lc-sys` crate (the TLS backend of the AWS SDK). Instead of
  installing NASM you can set `AWS_LC_SYS_PREBUILT_NASM=1`, which makes the
  crate use its prebuilt assembly objects. CI does this.
- Building `.msi` installers additionally needs the VBSCRIPT optional Windows
  feature, which is enabled by default.

## First build

```sh
pnpm install
cargo build --workspace
```

Cargo commands such as `cargo build`, `cargo clippy`, and `cargo test` do not
need a built UI: a debug build of `crates/desktop` (without Tauri's
`custom-protocol` feature) loads the interface from the Vite dev server at
`http://localhost:5173` instead of embedding `ui/dist`.

For the same reason, never ship or hand out a desktop executable from a plain
`cargo build`, not even with `--release`. Production executables and
installers come only from `pnpm tauri build`, run from the repository root:
it builds the UI first (the `beforeBuildCommand` in
`crates/desktop/tauri.conf.json` runs `pnpm --dir ui build`) and compiles the
app with the UI embedded. The `bb` CLI is unaffected and builds normally with
`cargo build -p bucketbridge-cli`.

## Everyday commands

| Task | Command |
|---|---|
| Format Rust code | `cargo fmt --all` (CI runs `cargo fmt --all --check`) |
| Lint Rust code | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| Run all Rust tests | `cargo test --workspace --locked` |
| Run tests whose name matches a filter | `cargo test -p bucketbridge-core <filter>` |
| Run the CLI | `cargo run -p bucketbridge-cli -- <arguments>` |
| Production desktop build and installers for this OS | `pnpm tauri build` (output under `target/release/bundle/`) |
| Type-check the UI | `pnpm --dir ui typecheck` |
| Lint the UI | `pnpm --dir ui lint` |
| UI unit tests (Vitest) | `pnpm --dir ui test` |
| Production UI build | `pnpm --dir ui build` |
| UI end-to-end tests (Playwright) | `pnpm --dir ui e2e` |
| Install the Playwright browser once | `pnpm --dir ui exec playwright install chromium` |

CI runs the formatting check, clippy, all tests, and every UI command in this
table. Clippy warnings fail the build, so run clippy before pushing.

## Working on the UI

There are two ways to run the interface:

- **In a browser:** `pnpm --dir ui dev` starts the Vite dev server. Outside
  Tauri the UI runs against the in-memory mock backend in `ui/src/mock`, so no
  Rust build, credentials, or S3 endpoint are needed. URL options of the mock
  (for example an empty or a locked account store) are described in
  `ui/src/mock/index.ts`. The Playwright tests use the same mode.
- **In the desktop shell:** `pnpm tauri dev`, run from the repository root,
  starts the Vite dev server (through the `beforeDevCommand` in
  `crates/desktop/tauri.conf.json`), builds `crates/desktop`, and opens the
  real app, which talks to S3 through `bucketbridge-core`. Use this for
  anything that crosses the IPC boundary described in `docs/desktop-api.md`.

## Integration tests

`bucketbridge-tests` holds the integration tests. By default they run
against an S3 server started inside the test process (s3s with a temporary
directory as storage), so `cargo test --workspace` needs no external
services.

To run the same tests against MinIO, start Docker and enable the `minio`
feature. The harness starts MinIO with testcontainers:

```sh
BB_TEST_BACKEND=minio cargo test -p bucketbridge-tests --features minio --locked
```

CI runs this on Linux in the `integration-minio` job.

### The 5 GiB multipart test

The round trip of a 5 GiB object through multipart upload and download is
opt-in because it is slow and needs about 15 GB of free disk space (the
object exists in MinIO's storage and in local files at the same time). Use
an optimised build; checksumming 5 GiB in a debug build is very slow:

```sh
BB_TEST_BACKEND=minio BB_TEST_LARGE=1 \
  cargo test --release -p bucketbridge-tests --features minio --locked
```

Run it by hand before a release; it needs Docker and about 11 GiB of free disk.

## Licence audit

Every dependency is licence-checked.
Install the tools once:

```sh
cargo install --locked cargo-deny
cargo install --locked cargo-about --features cli
```

Then, from the repository root:

```sh
# Rust: advisories, banned and duplicate crates, licences, sources (deny.toml)
cargo deny check

# Rust: the notice file shipped with every release (about.toml, about.hbs)
cargo about generate --locked --workspace --fail -o target/THIRD_PARTY_LICENSES.html about.hbs

# npm: licences of the production dependencies of the UI
mkdir -p target
pnpm --recursive licenses list --prod --json > target/npm-licenses.json
node scripts/ci/check-npm-licenses.mjs target/npm-licenses.json
```

The three allowlists must stay identical: `[licenses] allow` in `deny.toml`,
`accepted` in `about.toml`, and `ALLOWED` in
`scripts/ci/check-npm-licenses.mjs`. `docs/packaging.md` explains how to deal
with a failing check.

## Continuous integration

There is no CI on push or pull request: run the checks above locally before
committing. The only workflow is `release.yml` (`v*` tags, or a manual dry
run), which builds the platform artefacts, writes `SHA256SUMS`, and creates a
draft GitHub release (see `docs/packaging.md`). It runs with read-only
repository permissions; only the job that creates the draft release can write.
