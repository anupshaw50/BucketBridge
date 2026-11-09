# Roadmap

Ideas for future BucketBridge releases, grouped by priority. Sizes are rough:
**S** is a few days, **M** about a week or two, **L** several weeks. Nothing
here is promised for a particular release; open an issue to discuss an item or
to suggest a new one.

What is already built is listed in the [README](README.md#features).

## Done in 0.2.0

Priority 1 (access and data movement) shipped in version 0.2.0: AWS IAM
Identity Center (SSO), assume role, AWS shared configuration profiles,
instance and container roles, cross-account copy and move, saved sync jobs,
sync improvements, the overwrite policy, kept modification times and edit in
place. See the [changelog](CHANGELOG.md#020---2026-11-03).

## Done on main (next release)

Priority 2 (everyday use) is built and will ship in the next release: drag
and drop to copy, move and upload, a filtered and paged bucket list, natural
sorting with remembered sort order and column widths, an editable `s3://`
address bar, a "Copy as" menu, `bb://` links, account folders and secret-free
account export and import, a system tray with taskbar and Dock progress,
keeping the computer awake during transfers, a quit prompt while tasks run,
remembered window size and position, richer previews (image zoom, CSV and
Parquet tables, XML, GIF), bucket usage (`bb du`), a better versions dialog
and automatic log cleanup. See the [changelog](CHANGELOG.md#unreleased).

## Priority 3: buckets and storage

| # | Item | Size |
|---|---|---|
| 25 | Bucket default encryption (SSE-S3, SSE-KMS with bucket key). | S |
| 26 | Bucket tags for cost allocation. | S |
| 27 | A full ACL grant editor: grant by account ID or email, permission by permission. | M |
| 28 | Advanced options when creating a bucket: object lock, ownership, public access block, region. | S |
| 29 | Enable object lock on an existing bucket. | S |
| 30 | Cost estimate per retrieval tier before a Glacier restore. | M |
| 31 | Restore of objects archived by Intelligent-Tiering. | S |
| 32 | Lifecycle rules: copy and paste, JSON import and export, a structured editor next to the JSON one. | M |
| 33 | Upload rules: default headers, content type, storage class and encryption chosen by file pattern. | M |
| 34 | Resume an unfinished multipart upload from the multipart uploads tool. | M |
| 35 | Clone a bucket (objects and settings). | M |
| 36 | S3 Access Points and Multi-Region Access Points as targets (by ARN). | M |
| 37 | Transfer-acceleration endpoint for transfers (per account), and dual-stack endpoints. | S |
| 38 | Settings for S3-compatible providers: custom storage classes and regions, switches for multi-object delete and multipart copy, listing buckets across regions. | M |
| 39 | Per-account proxy, and an explicit per-account option to skip TLS certificate checks. | S |
| 40 | Accounts with limited permissions, such as write-only access without listing. | M |
| 41 | Link generator: CloudFront or custom host names, HTML snippets for images, choice of shortener. | S |
| 42 | Up-to-date regions and partitions (sovereign cloud, China, GovCloud). | S |

## Priority 4: command line

| # | Item | Size |
|---|---|---|
| 43 | `bb ls --versions` with filters, and `bb rm --versions` for bulk cleanup of old versions. | S |
| 44 | Wildcards and file masks in `cp`, `rm` and `ls`. | S |
| 45 | `bb sync` asks before deleting (`--yes` to skip), with per-file progress and a summary. | S |
| 46 | `bb account update-credentials` to rotate keys. | S |
| 47 | `bb version --check` to look up the latest release when asked (never automatically). | S |

