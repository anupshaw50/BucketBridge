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

## Done in 0.3.0

Priority 2 (everyday use) shipped in version 0.3.0: drag
and drop to copy, move and upload, a filtered and paged bucket list, natural
sorting with remembered sort order and column widths, an editable `s3://`
address bar, a "Copy as" menu, `bb://` links, account folders and secret-free
account export and import, a system tray with taskbar and Dock progress,
keeping the computer awake during transfers, a quit prompt while tasks run,
remembered window size and position, richer previews (image zoom, CSV and
Parquet tables, XML, GIF), bucket usage (`bb du`), a better versions dialog
and automatic log cleanup. See the [changelog](CHANGELOG.md#030---2026-11-09).

## Done on main (next release)

Priority 3 (buckets and storage) is built and will ship in the next release:
bucket default encryption and tags, a full ACL grant editor, advanced options
when creating a bucket, turning on Object Lock for an existing bucket, a cost
estimate before a Glacier restore, restore of Intelligent-Tiering archives, a
lifecycle form editor with copy, paste and JSON import and export, upload rules
by file pattern, resuming an unfinished multipart upload, cloning a bucket,
access points and Multi-Region Access Points by ARN, Transfer Acceleration and
dual-stack endpoints, settings for S3-compatible providers, a per-account proxy,
accounts that cannot list buckets, public links on a CloudFront or custom host
with HTML and Markdown snippets, and up-to-date regions and partitions. See the
[changelog](CHANGELOG.md#unreleased).

Still open from Priority 3:

| # | Item | Size |
|---|---|---|
| 39 | A custom per-account proxy with a password (kept in the secret store), and an explicit per-account option to skip TLS certificate checks for self-hosted servers. | S |

## Priority 4: command line

| # | Item | Size |
|---|---|---|
| 43 | `bb ls --versions` with filters, and `bb rm --versions` for bulk cleanup of old versions. | S |
| 44 | Wildcards and file masks in `cp`, `rm` and `ls`. | S |
| 45 | `bb sync` asks before deleting (`--yes` to skip), with per-file progress and a summary. | S |
| 46 | `bb account update-credentials` to rotate keys. | S |
| 47 | `bb version --check` to look up the latest release when asked (never automatically). | S |

