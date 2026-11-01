# Roadmap

Ideas for future BucketBridge releases, grouped by priority. Sizes are rough:
**S** is a few days, **M** about a week or two, **L** several weeks. Nothing
here is promised for a particular release; open an issue to discuss an item or
to suggest a new one.

What is already built is listed in the [README](README.md#features).

## Priority 1: access and data movement

| # | Item | Details | Size |
|---|---|---|---|
| 1 | AWS IAM Identity Center (SSO) | Browser sign-in with SSO session profiles and cached tokens, including GovCloud. | L |
| 2 | Assume role | Accounts that assume a role from a source account, with session name, duration and MFA. | M |
| 3 | AWS shared configuration | Use the profiles in `~/.aws/config` and `~/.aws/credentials` as accounts. | M |
| 4 | Instance and container roles | Credentials from EC2 (IMDSv2) and ECS roles, for use on servers. | S |
| 5 | Cross-account copy and move | Copy to and move to between accounts, not only within one. | M |
| 6 | Saved sync jobs | Named sync setups, run from the app or with `bb sync --job <name>`; exclusion rules loaded from a file. | M |
| 7 | Sync improvements | Bucket-to-bucket sync, skip empty folders, skip duplicate content, a local metadata cache for faster rescans, parallel analysis, a summary report. | L |
| 8 | Overwrite policy | For an existing destination: ask, overwrite, skip, overwrite if newer, or rename automatically. | S |
| 9 | Keep modification times | Downloads keep the object's last-modified time. | S |
| 10 | Edit in place | Open an object, edit it locally, and upload the change automatically when the file is saved. | M |

## Priority 2: everyday use

| # | Item | Size |
|---|---|---|
| 11 | Drag files and folders between panes and buckets to copy or move them. | M |
| 12 | Filter the bucket list by name, creation date and region; custom page size for large bucket lists. | S |
| 13 | Natural sorting (`file2` before `file10`); remember sort order and column widths. | S |
| 14 | Editable address bar that accepts a pasted `s3://` path. | S |
| 15 | A "Copy as" menu: name, key, `s3://` URI, virtual-hosted URL, path-style URL. | S |
| 16 | `bb://` links that open the app at a bucket and folder. | M |
| 17 | Account folders, reordering, and import and export of accounts (without secrets). | M |
| 18 | System tray, taskbar and Dock progress, and keeping the computer awake during transfers. | M |
| 19 | On quit: finish running tasks first, or ask when tasks are active. | S |
| 20 | Remember window size and position. | S |
| 21 | Preview: zoom and pan for images, table view for CSV and Parquet with paging, XML formatting, GIF. | M |
| 22 | Properties: total bucket size, version sizes, size of unfinished multipart uploads. | M |
| 23 | Versions: flat view, sortable columns, select the newest delete markers, select all non-current versions. | S |
| 24 | Event log: remove old log files automatically. | S |

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

