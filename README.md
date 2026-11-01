# BucketBridge

A cross-platform (Windows, macOS, Linux), open-source S3 client: a desktop app
(Tauri 2 + React) and a command-line tool (`bb`) built on one shared Rust core.
Works with Amazon S3 and S3-compatible services (custom endpoint, path-style
addressing).

## Status

What works today:

| Area | What works |
|---|---|
| Accounts | Multiple accounts, custom endpoints, path-style, requester pays, MFA (STS sessions), HTTP and SOCKS5 proxies with bypass list. Secrets live in the OS keychain; an optional master password encrypts them (Argon2id + AES-256-GCM). |
| Browsing | Bucket tree, folders, flat browsing, search, paging; bucket regions resolved automatically. |
| Objects | Upload and download of files and folders, copy, move, rename, delete, new folder, open, open with, copy to and move to a chosen folder (also whole folders), website redirect, share links (presigned GET and PUT) with a sharing wizard and optional URL shortening. |
| Transfers | Queue with configurable concurrency, pause, resume, cancel and retry, a shared bandwidth cap, multipart uploads with checksums, resume after an app restart. A cancelled transfer leaves no partial object or file. |
| Sync | Dry-run analysis with a per-row result (copy, delete, equal, failed), upload, download or bidirectional, resumable execution that never stops at the first failure. |
| App shell | Settings, several windows, bookmarks, open a bucket in the browser, event log (filter, export), local crash reports, single instance. No telemetry, nothing phones home. |
| Object metadata | Properties editor (HTTP headers, user metadata, tags, storage class, server-side encryption incl. SSE-C, canned ACLs) with batch editing and a result per object; `bb tag`, `bb storage-class`, `bb sse`, `bb acl`. |
| CLI `bb` | `account`, `ls`, `mb`, `rb`, `rm`, `cp`, `mv`, `sync`, `presign`, `mpu`, `tag`, `storage-class`, `sse`, `acl`, `lock`, `restore`, `encrypt`, `decrypt`, `iam`, `cloudfront`, `scan`, `export`, `restore-version`, `mount` (optional build); client-side encryption on `cp` and `mv`; `presign --shorten`; NDJSON output with `--json`; stable exit codes (0 ok, 1 usage, 2 remote, 3 auth). |
| Delivery | Release workflow for `.msi`, portable `.zip`, universal `.dmg`, `.AppImage`, `.deb`, `.tar.gz` and `bb` archives with `SHA256SUMS`; licence audit with cargo-deny and cargo-about. |
| Bucket configuration | Policy editor with validation, public access block, versioning, access logging, static website and routing rules, transfer acceleration; pinned external buckets. |
| Versions and preview | Version list with filters and delete markers, restore and purge, version download; preview of images, text and JSON with paging. |
| Tools | Connection diagnostics, unfinished multipart uploads, saved filters, export to CSV, Excel or Parquet (`bb export`). |
| Security scan | Flags public ACLs and policies, public access block, missing encryption, versioning off and ACLs enabled, per bucket, with an export (`bb scan`, Tools, Security scan). |
| Bucket rules | CORS, lifecycle, Intelligent-Tiering and replication rules, edited as JSON with validation. |
| Object lock and archive | Object lock (retention, legal hold, bucket default), object ownership, MFA delete, Glacier restore; `bb lock`, `bb restore`. |
| Client-side encryption | Optional compression plus AES-256-GCM encryption of objects before upload, decryption on download (Encrypt, Decrypt in the file menu). |
| IAM | IAM manager: users, console passwords, access keys, managed and inline policies. |
| CloudFront | Distributions per bucket: create, edit, enable or disable, detach, invalidations and their history. |
| Context menus | Right-click menus for buckets, folders, files, accounts and empty space, sharing one action registry with the toolbar and shortcuts. |

Mounting a bucket as a drive is a read-only optional build for Linux, macOS and Windows
(`--features mount`, needs libfuse, macFUSE or the Dokan runtime, see
[`docs/mount.md`](docs/mount.md)); default builds do not include it. The Windows adapter
(a drive letter or folder through Dokan) is type-checked but not yet run on a Windows
machine. Copy to and move to
stay within one account.

## Build and run

Prerequisites: Rust (stable), Node.js and pnpm, plus the Tauri system packages
on Linux. Details per OS are in [`docs/development.md`](docs/development.md).

```bash
pnpm install

# Desktop app with hot reload (Vite needs port 5173 to be free)
pnpm tauri dev

# Desktop app without a dev server
pnpm tauri build --debug --no-bundle
./target/debug/bucketbridge

# The UI alone in a browser, with a built-in fake backend (no S3 needed)
pnpm --dir ui dev

# The CLI
cargo build -p bucketbridge-cli
./target/debug/bb --help
```

Set `BB_CONFIG_DIR=/some/folder` to keep configuration, logs, the transfer
queue and sync plans in a separate folder (useful for trying things out).

## Test

```bash
cargo test --workspace     # Rust: unit tests plus an in-process S3 server
pnpm --dir ui test         # UI unit and component tests
pnpm --dir ui e2e          # Playwright smoke tests against the fake backend
```

Tests that need a full S3 implementation run against MinIO (Docker); see
[`docs/testing.md`](docs/testing.md).

## Documentation

| Document | Contents |
|---|---|
| [`docs/architecture.md`](docs/architecture.md) | Workspace layout, module map, error model, defaults |
| [`docs/development.md`](docs/development.md) | Prerequisites, building, testing, linting |
| [`docs/testing.md`](docs/testing.md) | Test harness and backends |
| [`docs/desktop-api.md`](docs/desktop-api.md) | Contract between the desktop host and the UI |
| [`docs/desktop.md`](docs/desktop.md) | Desktop host: state, commands, events, security policy |
| [`docs/ui.md`](docs/ui.md) | UI structure, fake backend, tests |
| [`docs/cli.md`](docs/cli.md) | `bb` commands, records, exit codes |
| [`docs/engines.md`](docs/engines.md), [`docs/transfers.md`](docs/transfers.md) | Transfer engines and the queue |
| [`docs/sync.md`](docs/sync.md) | Sync rules and execution |
| [`docs/security.md`](docs/security.md) | Secrets, master password, MFA, proxies |
| [`docs/logging.md`](docs/logging.md) | Event log and crash reports |
| [`docs/packaging.md`](docs/packaging.md) | Releases, signing, licence audit |

## License

MIT. See [LICENSE](LICENSE).
