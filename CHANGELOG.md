# Changelog

All notable changes to BucketBridge are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Downloads for every
version are on the [Releases page](https://github.com/anupshaw50/BucketBridge/releases).

## [Unreleased]

### Added

- Project website at <https://anupshaw50.github.io/BucketBridge/>, deployed with
  GitHub Pages.
- Screenshots in the README and on the website.
- Package manifests for winget (`AnupShaw.BucketBridge`, `AnupShaw.bb`),
  Scoop (`bucketbridge`, `bb`) and Homebrew (cask `bucketbridge`, formula
  `bb`), regenerated for each release by `scripts/ci/package-manifests.mjs`.
- Contributing guide, security policy, code of conduct, issue forms and a
  pull request template.
- Roadmap of planned features (`ROADMAP.md`).
- This changelog.

### Changed

- Copyright holder is Anup Shaw (anupshaw50) in the app, the installers and
  `bb.exe`.
- README: install instructions per platform, trademark and author sections.

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

[Unreleased]: https://github.com/anupshaw50/BucketBridge/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/anupshaw50/BucketBridge/releases/tag/v0.1.0
