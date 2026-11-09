# Changelog

All notable changes to BucketBridge are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Downloads for every
version are on the [Releases page](https://github.com/anupshaw50/BucketBridge/releases).

## [Unreleased]

## [0.3.0] - 2026-11-09

### Added

- **Simplified Chinese** for the desktop app and `bb`, chosen from the system
  language (English stays the default); Settings > Interface > Language and
  `BB_LANG` switch it. Contributed by @carnothuang.
- `bb` is included in the Linux `.deb` package.
- **Drag and drop:** drag objects to copy them between folders, buckets,
  accounts and windows (hold Command on macOS or Shift elsewhere to move), and
  drop files from the file manager to upload them.
- **Bucket list:** filter by name, region and creation date, and a page size
  for very long lists.
- **Sorting:** natural order (`file2` before `file10`) for buckets and
  objects; the sort order and column widths are remembered.
- **Address bar:** type or paste an `s3://bucket/prefix` path (Ctrl/Cmd+L).
- **Copy as:** name, key, `s3://` URI, virtual-hosted URL or path-style URL,
  one line per selected item.
- **`bb://` links** open the app at an account, bucket and folder.
- **Account folders and order**, and **export and import of accounts without
  secrets** (`bb account export`, `bb account import`, `bb account folder`,
  `bb account move`).
- **System tray** with pause, resume and transfer progress; progress on the
  Windows taskbar and the macOS Dock; the computer stays awake while
  transfers or a sync run (setting).
- **Quit while tasks run:** ask, finish the tasks first, or quit now
  (setting).
- **Window size, position and maximised state** are remembered and kept on a
  connected screen.
- **Preview:** zoom and pan for images, CSV, TSV and Parquet as paged tables,
  formatted XML (entities are never expanded), animated GIF.
- **Bucket usage:** total size and object count, non-current versions, delete
  markers and unfinished multipart uploads, in the bucket properties and with
  `bb du`.
- **Versions dialog:** flat view, sortable columns, select the newest delete
  markers or all non-current versions.
- **Event log:** old log files are removed automatically by age and total
  size.
- `AGENTS.md` with the project rules and the review checklist for people and
  AI coding agents.

## [0.2.0] - 2026-11-03

### Added

- **Credential sources.** An account can now get its credentials from
  somewhere other than stored keys:
  - **AWS profiles** from `~/.aws/config` and `~/.aws/credentials`
    ("Import from AWS config", `bb account import-aws`, `--profile NAME`).
  - **AWS IAM Identity Center (SSO)**: sign in through the browser, pick an
    account and role; the sign-in is shared with the AWS CLI
    (`bb account sso-login`).
  - **Assume role** from another account, with session name, duration,
    external ID and MFA; credentials renew before they expire.
  - **EC2 and ECS instance roles**, for `bb` on servers.
- **Cross-account copy and move**: choose the destination account in the
  picker or with `bb cp/mv --from-account A --to-account B`. A server-side copy
  is used where possible, otherwise the data streams through this machine. A
  move deletes the source only after the copy is verified.
- **Overwrite policy** for uploads, downloads, copies and moves: overwrite,
  skip, overwrite if newer, rename, or ask (with "apply to all"); skipped
  items are reported. CLI `--if-exists`.
- **Modification times are kept**: downloads take the original file time,
  stored on upload as `x-amz-meta-mtime` (CLI `--no-preserve-mtime`).
- **Edit in place**: open an object, edit it, and each save is uploaded,
  with a check that nobody changed the object in the meantime (`bb edit`).
- **Saved sync jobs**: name a sync setup and run it again from the app or
  with `bb sync --job NAME`; gitignore-style exclusion files
  (`--exclude-from`).
- **Sync options**: bucket-to-bucket sync, skip empty folders, skip
  duplicate content, a local listing cache for faster rescans, parallel
  listing, and a summary report at the end (`--summary`).
- About dialog: author links and the full GPL-3.0 text.
- Project website, screenshots, package manifests for winget, Scoop and
  Homebrew, contributing guide, security policy, code of conduct, issue
  forms, roadmap and this changelog.

### Changed

- Copyright holder is Anup Shaw (anupshaw50) in the app, the installers and
  `bb.exe`.
- Credential-provider failures are reported as authentication errors.

### Security

- Updated `calamine` (RUSTSEC-2026-0436, used only in tests) and
  `async-executor` (yanked release).

## [0.1.0] - 2026-11-01

First public release.

### Added

- **Desktop app** for Windows, macOS and Linux (Tauri 2 with a React UI), and
  the **`bb` command-line tool**, both built on one Rust core.
- **Accounts:** several accounts at once, custom endpoints and path-style
  addressing for S3-compatible services, requester pays, MFA through STS
  session tokens, HTTP and SOCKS5 proxies with a bypass list.
- **Secrets** in the OS keychain, with an optional master password
  (Argon2id and AES-256-GCM).
- **Browsing:** bucket tree, folders, flat view, search, paging, bookmarks,
  several windows, context menus and keyboard shortcuts.
- **Objects:** upload and download of files and folders, copy, move, rename,
  delete, open and open with, copy to and move to a chosen folder, presigned
  links with a sharing wizard and optional URL shortening.
- **Transfer queue:** concurrency limit, pause, resume, retry and cancel,
  a shared bandwidth cap, multipart transfers with checksums, resume after a
  restart; a cancelled transfer leaves nothing behind.
- **Folder sync:** upload, download or both ways, with a dry-run analysis and
  resumable execution.
- **Object settings:** HTTP headers, user metadata, tags, storage class,
  server-side encryption (SSE-S3, SSE-KMS, SSE-C), ACLs, website redirect,
  object lock and legal hold, Glacier restore.
- **Client-side encryption** with optional gzip or zstd compression.
- **Versions and preview:** version list with filters, restore and purge;
  preview of images, text and JSON.
- **Bucket settings:** policy editor with validation, public access block,
  versioning, logging, static website, transfer acceleration, CORS,
  lifecycle, Intelligent-Tiering, replication, object ownership, MFA delete.
- **IAM manager** (users, console passwords, access keys, policies) and
  **CloudFront** (distributions per bucket, invalidations).
- **Tools:** security scan, connection diagnostics, unfinished multipart
  uploads, saved filters, export of listings to CSV, Excel or Parquet.
- **Mount as a drive** (read-only, optional build with `--features mount`).
- `bb` commands for accounts, listing, copy, move, sync, presigned URLs,
  metadata, object lock, encryption, IAM, CloudFront, security scan and
  export, with NDJSON output and stable exit codes.
- Event log with export, local crash reports, light and dark theme, no
  telemetry.
- Release artefacts: Windows `.msi` and portable `.zip`, universal macOS
  `.dmg`, Linux `.AppImage`, `.deb` and `.tar.gz`, `bb` archives for each
  platform, `SHA256SUMS` and third-party licence notices.
- Licensed under GPL-3.0-only.

[Unreleased]: https://github.com/anupshaw50/BucketBridge/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/anupshaw50/BucketBridge/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/anupshaw50/BucketBridge/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/anupshaw50/BucketBridge/releases/tag/v0.1.0
