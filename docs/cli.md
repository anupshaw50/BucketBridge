# `bb` command-line client

`bb` is the scriptable client of BucketBridge (`crates/cli`, built as the
`bb` binary). This page describes this version: stored accounts, listing,
bucket management, deletes, uploads, downloads, server-side copies and moves,
folder sync, presigned URLs, uncompleted multipart uploads and the export of a
listing to CSV, Parquet or Excel.
folder sync, presigned URLs, uncompleted multipart uploads and restoring or
removing single object versions.
folder sync, presigned URLs, uncompleted multipart uploads and object metadata
(tags, storage class, encryption, ACLs).

```bash
cargo run -p bucketbridge-cli -- ls s3://my-bucket/photos/
```

The crate is a library (`bucketbridge_cli`) with a thin binary on top: the
binary calls `bucketbridge_cli::run` with the real process streams, the OS
keychain and Ctrl-C, and tests call it in-process with captured streams and
an in-memory secret store.

## Accounts

A command runs as one account, chosen in this order:

1. `--account <LABEL|ID>`: a stored account, by ID (exactly) or by label
   (case-insensitively). `environment` names the account built from the
   `BB_*` variables, unless a stored account carries that label. Any other
   name is `not_found` (exit 2).
2. The default stored account (`*` in `bb account list`).
3. The `environment` account.

The `environment` account is read from these variables:

| Variable | Meaning |
|---|---|
| `BB_ACCESS_KEY_ID` | access key ID (required) |
| `BB_SECRET_ACCESS_KEY` | secret access key (required) |
| `BB_SESSION_TOKEN` | session token for temporary credentials |
| `BB_REGION` | signing region, default `us-east-1` |
| `BB_ENDPOINT` | custom endpoint URL for S3-compatible providers |
| `BB_PATH_STYLE` | path-style addressing: `true`/`false` (also `1`/`0`, `yes`/`no`, `on`/`off`) |

Missing keys are an `auth` error (exit 3). AWS profiles, `AWS_*` variables
and instance metadata are never used. The HTTP client honours `HTTP_PROXY`,
`HTTPS_PROXY` and `NO_PROXY`.

`BB_SHORTENER_TOKEN` and `BB_SHORTENER_ENDPOINT` only matter for `bb presign --shorten`.

`BB_LANG` selects the language of the help and the output: a value beginning
with `zh` gives Simplified Chinese, anything else gives English, which is also
the default. When `BB_LANG` is not set, `LC_ALL`, `LC_MESSAGES` and `LANG` are
consulted in that order.

### Stored accounts and their secrets

Stored accounts live in `config.toml`; their secret access keys and session
tokens live in the OS keychain (macOS Keychain, Windows Credential Manager,
the Secret Service on Linux), and `config.toml` only holds references to
them. Where no keychain is available (for example Linux without a Secret
Service), storing an account fails with an `io` error; use the
`environment` account there.

A secret is never a command-line argument, since arguments end up in the
shell history and in the process list. `bb account add` reads the secret
access key from, in this order:

1. standard input with `--secret-stdin`: the first line is the secret
   access key, an optional second line a session token;
2. `BB_SECRET_ACCESS_KEY` (and `BB_SESSION_TOKEN`), for automation;
3. a prompt on the terminal, without echo (`Secret access key: `).

Without any of them (no terminal, nothing on stdin) it is a usage error.

```bash
bb account add --label work --access-key-id AKIAEXAMPLE --region eu-west-1 --secret-stdin < secret.txt
bb account add --label minio --access-key-id admin --endpoint https://minio.example.com --path-style
```

A master password (set in the desktop application) encrypts the stored
secrets. Commands that need a secret (`account add`, and any command that
runs as a stored account) then ask for it on the terminal
(`Master password: `), or read `BB_MASTER_PASSWORD`. Without either the
secrets stay locked: a `locked` error (exit 3). A wrong password is an
`auth` error (exit 3). `account list`, `account remove` and `account
default` need no password.

`--mfa-code CODE` logs in with the current code of the account's MFA device
(`--mfa-serial` of `account add`) before the command runs. The MFA session
lives in memory for that one invocation, so every command of an account
with an MFA device needs a fresh code; without one it fails with
`mfa_required` (exit 3). With the `environment` account `--mfa-code` is a
usage error.

### `bb account add|list|remove|default|import-aws|sso-login`

| Command | Effect |
|---|---|
| `bb account add --label L --access-key-id ID [--region R] [--endpoint URL] [--path-style] [--request-payer] [--mfa-serial SERIAL] [--secret-stdin]` | Stores an account (secrets as above). Labels are unique, case-insensitively; a taken label is a `conflict`. The first account becomes the default. |
| `bb account add --label L --assume-role ARN --source-account LABEL\|ID [--session-name NAME] [--duration DURATION] [--external-id ID] [--mfa-serial SERIAL]` | Stores an account that assumes a role through another stored account (see [Credential sources](#credential-sources)). |
| `bb account add --label L --sso --sso-start-url URL --sso-region R --sso-account-id ID --sso-role-name NAME [--sso-session NAME]` | Stores an account that uses a role of IAM Identity Center. |
| `bb account add --label L --instance-role [--no-probe]` | Stores an account that uses the role of this EC2 instance or container. It first checks that the role exists (`auth`, exit 3, when not), unless `--no-probe`. |
| `bb account import-aws [--profile NAME]...` | Links profiles of the AWS config files as accounts (no key is copied). Without `--profile`, every usable profile that is not linked yet. |
| `bb account sso-login [LABEL\|ID] [--start-url URL --sso-region R] [--sso-session NAME] [--no-browser]` | Signs in to IAM Identity Center (device authorization). |
| `bb account list` | One line per account: `*` for the default, label, region, endpoint (`-` for AWS), credential source (`keys`, `aws-profile`, `assume-role`, `sso`, `instance-role`), ID. |
| `bb account remove LABEL\|ID` | Removes the account and its secrets. Removing the default makes the first remaining account the default. |
| `bb account default LABEL\|ID` | Makes the account the default. |

The account is an argument of these commands; `--account` and `--mfa-code`
are usage errors with them.

All endpoint options of `account add` (`--region`, `--endpoint`, `--path-style`,
`--request-payer`) also apply to the accounts below. An account that gets its
credentials from another source has no secret: `--access-key-id` and
`--secret-stdin` are not allowed with `--assume-role`, `--sso` or
`--instance-role` (usage error).

### Credential sources

| Source | How to use it |
|---|---|
| AWS profile | `bb account import-aws --profile dev` links the profile `dev` of `~/.aws/config` and `~/.aws/credentials` (or `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE`) as an account. The files stay the source of the keys; the account stores only the profile name. |
| One-off profile | `bb --profile dev ls` runs a command with a profile without storing it. `BB_REGION`, `BB_ENDPOINT` and `BB_PATH_STYLE` apply as for the environment account (the region defaults to the profile's). `--profile` cannot be combined with `--account` or `--mfa-code`, and is given once (several times only to `import-aws`). |
| Assumed role | `bb account add --label prod --assume-role arn:aws:iam::123456789012:role/Admin --source-account work --duration 2h --external-id ID`. The session name defaults to `bucketbridge`; the duration (`900` seconds to `12h`, default `1h`) accepts the usual duration forms. The session refreshes before it expires. With `--mfa-serial` the role asks for a code: `bb --account prod --mfa-code 123456 ls`. |
| IAM Identity Center | `bb account sso-login --start-url https://example.awsapps.com/start --sso-region eu-west-1` prints the verification URL and code, opens the browser when run in a terminal (not with `--no-browser` or `--json`), waits for the approval and stores the token in the AWS CLI cache. Then add the account with `bb account add --sso ...`. `bb account sso-login LABEL` signs in to the portal of a stored SSO account. |
| Instance or container role | `bb account add --label ec2 --instance-role` on an EC2 instance, ECS or EKS container. |

`sso-login` records with `--json`: first `{"user_code", "verification_uri",
"verification_uri_complete", "expires_in_secs"}`, then `{"start_url",
"sso_region", "expires_at"}`. `import-aws` prints one account record per
imported profile and, for a profile that was left out, `{"profile", "reason",
"skipped": true}`; it exits 1 when profiles were named and none could be
imported. Exit codes follow [Exit codes](#exit-codes): a sign-in that was denied
or has expired, a missing sign-in and a machine without a role are `auth`
(exit 3).

## Configuration

Transfer, retry, timeout, checksum and listing page-size settings come from
`config.toml` (see `docs/architecture.md`): `--config-dir`, else
`BB_CONFIG_DIR`, else the platform directory. A missing file means the
defaults; a file that exists but cannot be read is an error (exit 2). Only
`bb account` changes the file, through the same locked read-modify-write as
the desktop application.

## Global options

These work before or after the command name.

| Option | Effect |
|---|---|
| `--account <LABEL\|ID>` | Account to use (see [Accounts](#accounts)). |
| `--profile <NAME>` | Use a profile of the AWS config files without storing it (see [Credential sources](#credential-sources)); for `bb account import-aws`, the profiles to import (repeatable). |
| `--mfa-code <CODE>` | Log in with the account's MFA device first. |
| `--json` | One JSON record per item on stdout (NDJSON); errors become JSON records on stderr. |
| `-q`, `--quiet` | Print nothing for successful changes (`cp`, `mv`, `sync`, `rm`, `mb`, `rb`, `mpu abort`, `export`, `account add\|remove\|default`) and no progress. Failures are still reported. Listings (`ls`, `presign`, `mpu list`, `account list`) are unaffected. |
| `--config-dir <DIR>` | Directory holding `config.toml`; overrides `BB_CONFIG_DIR`. |
| `-h`, `--help` / `-V`, `--version` | Help or version on stdout, exit 0. |

## Commands

S3 locations are written `s3://bucket/key`. A key ending in `/` (or a whole
bucket, `s3://bucket`) is a prefix, that is a folder. Anything else that
does not look like a URL is a local path.

### `bb ls [S3_URI] [-r] [--versions] [-l]`

Without a URI: every bucket, sorted by name. With a URI: the folders and
objects directly under the prefix (folders first), or with `-r`,
`--recursive` every object under it. For a prefix ending in `/`, its own
folder marker is not listed. A URI that does not end in `/` lists every key
that starts with it, the object with exactly that key (or its versions)
first, so `bb ls s3://bucket/report.pdf` shows that object. `--versions`
lists object versions and delete markers (always in the long format). Pages
are fetched with the `list_page_size` setting (default 1000).

| Format | Columns |
|---|---|
| buckets | `name`; with `-l`: creation date, region, name |
| objects | `key`; with `-l`: last modified, size (`PRE` for folders), storage class, key |
| versions | last modified, size (`DEL` for delete markers, `PRE` for folders), `latest` or `-`, version ID, key |

### `bb mb BUCKET [--region R]` and `bb rb BUCKET`

`BUCKET` is a name or `s3://bucket`. `mb` validates the name against the S3
rules (invalid names are usage errors, exit 1) and creates the bucket in
`--region`, by default in the account region on AWS and in the provider's
default region on a custom endpoint; an existing bucket is a `conflict`
(exit 2). `rb` removes an empty bucket; a bucket that still holds objects,
versions or delete markers is a `conflict` (exit 2) and nothing is deleted.

### `bb rm S3_URI [-r] [--versions] [--version-id ID] [--dry-run]`

* `bb rm s3://bucket/key` deletes one object; a missing object is
  `not_found` (exit 2).
* A prefix (or a whole bucket) needs `-r`, `--recursive`; `-r` needs a
  prefix ending in `/`. Keys are listed and deleted 1000 per request.
* `--versions` deletes every version and delete marker of the key, or of
  every key under the prefix with `-r`. On a versioned bucket a plain delete
  only adds a delete marker; `--versions` removes the data for good.
* `--version-id ID` deletes exactly one version or delete marker of one object for good
  (`S3_URI` is an object key, not a prefix; it cannot be combined with `-r` or `--versions`).
  A version that does not exist is `not_found` (exit 2). Removing the newest delete marker
  brings the previous version back.
* `--dry-run` prints what would be deleted and changes nothing.

### `bb restore-version S3_URI --version-id ID`

Makes an older version of an object its current version: a server-side copy of that version
over its key (the IDs are in `bb ls --versions`). Every version stays; the copy is a new
current one that keeps the content headers, user metadata and tags and asks again for the
storage class and the SSE-S3 or SSE-KMS encryption of the version. A delete marker (remove it
with `bb rm --version-id`), an SSE-C version and a version above 5 GiB are usage errors
(exit 1); restoring the current version is a `conflict` (exit 2); a missing version is
`not_found`. Prints `restored version V of s3://bucket/key (new version N)`.

### `bb cp SOURCE DESTINATION [-r] [--include GLOB] [--exclude GLOB] [--dry-run] [--if-exists POLICY] [--no-preserve-mtime] [--from-account A] [--to-account B]` and `bb mv ...`

Uploads (local to S3), downloads (S3 to local) and server-side copies
(S3 to S3). Two local paths are a usage error.

One file or object (without `-r`):

* Upload: a destination that is a bucket or ends with `/` receives the
  file's name (`photo.jpg` to `s3://b/backup/` gives
  `s3://b/backup/photo.jpg`); any other destination is the exact key. A
  folder needs `-r`.
* Download: an existing folder, or a path written with a trailing
  separator (`out/`), receives the object's name; any other path is the
  exact file. Missing parent folders are created.
* Copy: as for uploads, `s3://a/photos/x.jpg` to `s3://b/backup/` gives
  `s3://b/backup/x.jpg`. Copying an object onto itself is a usage error.
* A prefix as the source needs `-r`.

Folders and prefixes (`-r`, `--recursive`): the selected folder lands
under the destination with its own name.

* Upload: `photos` to `s3://b/backup/` gives `s3://b/backup/photos/...`
  (relative paths are kept; `.` and `..` stand for the folder they name).
  Empty folders become folder markers. Symbolic links to files are
  followed, links to folders are not (they are reported as failed items).
* Download: the source must be a prefix ending in `/` (or a bucket).
  `s3://b/photos/2024/` to `out` gives `out/2024/...`; a whole bucket puts
  its keys directly into the folder. Folder markers become folders. Keys
  that cannot be local paths (such as `..` segments or names this OS does
  not allow) are reported as failed items: nothing is ever written outside
  the destination folder. A prefix with nothing below it downloads nothing.
* Copy: `s3://a/photos/2024/` to `s3://b/backup/` gives
  `s3://b/backup/2024/...`; a whole bucket puts every key under the
  destination unchanged. A destination inside the source, or one that maps
  every key onto itself, is a usage error.
* Two items that would land on the same destination are a `conflict`
  (exit 2) before anything is transferred. For downloads, names are
  compared the way the local file system compares them (without regard to
  case on macOS and Windows), and a file where another object needs a
  folder (`a` next to `a/b`) counts as well.

`--include GLOB` / `--exclude GLOB` (with `-r`) filter by the path below the
selected folder or prefix, with `/` separators; folder markers and empty
folders end with `/` (`--exclude 'tmp/'`), and the selected folder itself is
always kept. Rules apply in command-line order, the last matching rule
wins, and paths no rule matches are included. In patterns, `*` matches any
characters except `/`, `**` matches any characters including `/`, `**/`
matches zero or more whole folders (`**/*.jpg` matches `a.jpg` and
`x/y/a.jpg`), `?` matches one character except `/`, `[abc]` and `{a,b}`
match one of the alternatives, `\` escapes the next character, and
everything else matches itself. Example: `--exclude '**' --include '*.jpg'`
copies only the JPEG files directly in the source folder.

How transfers run:

* Up to `max_concurrent_tasks` items transfer at once (records are still
  printed in plan order). Files of at least `multipart_threshold_mib` use a
  multipart upload with `part_size_mib` parts, `max_parts_per_task` of them
  in flight.
* `bandwidth_limit_kibps` caps all transfers of the invocation together.
* An item that fails with a `remote` or `io` error is tried again up to
  `retry_attempts` times (on top of the HTTP client's own retries), with
  exponential backoff; a multipart upload or a partial download continues
  where it stopped. An item that fails for good leaves nothing behind: its
  multipart upload is aborted and its partial file removed.
* Downloads write `<file>.bbpart` and rename it when complete, so the
  destination file never holds partial data. Names ending in `.bbpart` are
  reserved for these files: an object with such a name is not downloaded
  (and a destination like that is refused), and such local files are not
  uploaded from a folder.
* Progress (bytes, rate and items done) is drawn on stderr, only when
  stderr is a terminal and neither `--quiet` nor `--json` is given.

`mv` deletes each source only after its own transfer succeeded: a failed,
skipped or interrupted item keeps its source. Local folders that `mv` emptied are
removed, up to the selected folder; folders still holding excluded or
failed files stay.

`--dry-run` prints what would be transferred (`would upload ...`) and writes
nothing, locally or remotely; a single named source is still checked to
exist.

#### When the destination exists: `--if-exists`

`--if-exists overwrite|skip|newer|rename|ask` chooses what happens to an item
whose destination (object or local file) already exists; without it the
`transfer.if_exists` setting applies (default `overwrite`).

* `skip` keeps the destination. The item is reported as skipped, never
  silently: a `bb: skipped SOURCE to DEST: ...` line on stderr (also with
  `--quiet`), or with `--json` a record with `"copied": false` and a
  `skipped` reason. It is not a failure (exit code 0), and `mv` keeps the
  source of a skipped item.
* `newer` replaces the destination only when the source is newer: the
  local modification time against the object's `mtime` metadata (else
  `Last-Modified`); for S3 to S3 the same two object times. Times compare in
  whole seconds; an unknown time counts as not newer.
* `rename` writes to `name (1).ext`, `name (2).ext`, ... (the first free name);
  the record and the report name the destination that was used.
* `ask` (also from the setting) asks for each conflict on the terminal:
  `[o]verwrite, [s]kip, [r]ename, [n]ewer only, [c]ancel`; a capital letter
  applies the answer to all remaining conflicts of the command. It needs a
  terminal on standard input and standard error: elsewhere the command stops
  with a usage error (exit 1) before it transfers anything, so scripts never
  block. `--dry-run` does not check destinations.

`bb sync` has no `--if-exists`: its plan already decides what is written, and
its semantics are unchanged.

#### Modification times: `--no-preserve-mtime`

By default an upload stores the local modification time as the user metadata
`x-amz-meta-mtime` (Unix seconds, with a fraction when the time has one) and a
download sets the file's modification time to that value, or to the object's
`Last-Modified` when there is none, so a round trip keeps the original time.
`--no-preserve-mtime` (and the setting `data_integrity.preserve_mtime`) turns
both off.

#### Copies between accounts: `--from-account` and `--to-account`

`--from-account A` names the account that owns the S3 source (default: the
account of `--account`, the default account or the environment account) and
`--to-account B` the one that owns the S3 destination; each takes a label, an
ID or `environment`. They also work with uploads (`--to-account`) and
downloads (`--from-account`); the other one is a usage error. With either on a
copy or move between S3 locations:

```bash
bb cp -r --from-account Work --to-account Archive s3://work-data/2026/ s3://archive/work/
bb mv    --from-account Work --to-account Archive s3://work-data/a.csv s3://archive/a.csv
```

When both accounts use the same endpoint, the destination account first asks
the service to copy (no data passes this machine; it needs read access to the
source). On `AccessDenied`, or when the endpoints differ, the object is
streamed through this machine with bounded memory, the checksums, resume and
the bandwidth limit of any transfer. Requester pays applies on both sides.
`mv` deletes a source only after the copy was read back and compared with it
(size, plain ETags, checksums of the same kind); a failed or unverifiable copy
keeps the source. The streaming route does not copy tags or ACLs. There is no
per-URI account syntax: the accounts are options of the command. MFA codes
(`--mfa-code`) apply to `--account` only.

#### Client-side encryption of uploads and downloads

`bb cp` and `bb mv` can encrypt on this machine before upload and decrypt
after download, with the envelope of `bucketbridge_core::cse` (see
`docs/security.md`); the server only sees ciphertext.

* `--encrypt` (uploads, also with `-r`): every file is encrypted with the
  password. `--compress none|gzip|zstd` (needs `--encrypt`, default none)
  compresses first. A local file that already is an encrypted envelope is
  refused (`conflict`, nothing is uploaded for it), so nothing is encrypted
  twice. The objects carry the `bb-cse` user metadata.
* `--decrypt` (downloads, also with `-r`): objects that are encrypted are
  decrypted after the download; other objects are saved as they are. The
  password is checked against the object's header before the body is
  downloaded; data that fails its integrity check is not kept.
* Without these options the data is transferred as it is (an encrypted
  object downloads as ciphertext).
* `--encrypt` and `--decrypt` conflict, apply only to the matching direction
  and not to S3 to S3 copies (use `bb encrypt` and `bb decrypt`); `--dry-run`
  does not ask for a password.
* The password is never a command-line argument. In order: the first line of
  `--password-file FILE`, the `BB_CSE_PASSWORD` environment variable, or a
  prompt on the terminal (asked twice when encrypting). With none of these
  it is a usage error (exit 1) before anything is transferred.
* Errors: a wrong password is `auth` with the message "password not valid"
  and a missing one "password not found" (exit 3); damaged, truncated or
  modified data is an `io` error saying it failed its integrity check
  (exit 2).

`bb sync` does not encrypt.

### `bb encrypt S3_URI [-r] [--compress ALGORITHM]` and `bb decrypt S3_URI [-r]`

Rewrite objects that are already in S3: download, encrypt (or decrypt) and
upload under the same key, keeping content type, headers, user metadata and
storage class (tags and ACL are not kept; on a versioned bucket the old
version stays). The upload is conditional on the ETag that was read, so an
object that changed meanwhile is never overwritten. The password comes as
for `cp` (`--password-file`, `BB_CSE_PASSWORD`, prompt) and is checked before
the download. Disk use is one extra copy of the object.

A prefix needs `-r`. With `-r`, objects that are in the wanted state already
(`encrypt`: encrypted, `decrypt`: not encrypted) are skipped
(`skipped s3://b/k (already encrypted)`); without `-r` they are errors
(`conflict` for `encrypt`, `usage` for `decrypt`). Folder markers are
ignored. Records: `bucket`, `key`, `action`, `size_before`, `size_after`,
`etag`, `skipped`, `error`.

### `bb sync LOCAL S3_URI [--direction DIRECTION] [--delete] [--dry-run]` and `bb sync --resume PLAN_ID`

Makes a local folder and an S3 prefix match. The local folder always comes
first and the S3 location second (swapping them is a usage error). A
relative folder is made absolute against the current directory.
`s3://bucket/photos` names the folder `photos/`, so `photos-old/...` is never
part of it; `s3://bucket` is the whole bucket. Only files are synced: empty
folders and folder markers are not (unless `--keep-empty-folders` is given).

| `--direction` | Effect |
|---|---|
| `upload` (default) | Makes the prefix match the folder: files that are missing in S3 or different are uploaded. |
| `download` | Makes the folder match the prefix: objects that are missing locally or different are downloaded (a missing folder is created). |
| `bidirectional` | A file that exists on one side only is copied to the other; of a pair that differs, the newer file is copied over the other one. |

A pair is different when the sizes differ or the source is newer by more than
2 seconds; content is not compared. When both sides changed and neither is
clearly newer (bidirectional), the pair is a conflict: it is reported as a
failed row and left alone. The rules (time tolerance, symbolic links,
unreadable files, names that cannot be keys or local paths, case collisions)
are those of `docs/sync.md`.

`--delete` also removes what exists only on the target side: with `upload`
the objects under the prefix that have no local file, with `download` the
local files that have no object (folders that this empties are removed too,
but never the synced folder itself). It cannot be combined with
`--direction bidirectional`, where a file missing on one side looks exactly
like a new file on the other (usage error). Nothing outside the prefix or the
folder is ever touched.

How a run goes:

1. The prefix is listed, the folder is scanned and compared, which gives a
   plan with one row per file that needs attention (`copy`, `delete`, or
   `failed` for a file that cannot be synced). Rows that are already `equal`
   produce no output; they are counted in the summary.
2. `--dry-run` prints the plan and stops: no file, object or stored plan is
   written, not even the plan store folder. It exits with 2 when the plan has
   failed rows, since a real run would fail on them too.
3. Otherwise the plan is stored (in `data/sync` below the config directory,
   or the platform data directory) and executed: local deletions first, then
   copies and remote deletions, `max_concurrent_tasks` at a time under the
   `bandwidth_limit_kibps` cap, as for `cp` ("How transfers run" above). A
   row that fails never stops the others, and a failed row is not tried again
   during the run (beyond the HTTP client's own retries).
4. Each row is reported as it finishes, which with concurrent transfers is
   not path order, and the run ends with a summary. Downloads write
   `<file>.bbpart` and rename it, as for `cp`. Progress (rows and bytes of the
   finished rows) is drawn on stderr only on a terminal, without `--quiet`
   and `--json`.

Plans and `--resume`:

* A plan whose rows all finished is removed from the store again, and nothing
  is stored when there is nothing to execute: a run right after a successful
  one only reports `0 copied, 0 deleted, N already in sync`.
* A plan that was interrupted (Ctrl-C) or has rows that failed while running
  stays in the store, and `bb sync --resume PLAN_ID` runs its rows that are
  not done again. The plan ID is in the records and in the hints (see
  [Output](#output)).
* The folder, prefix, direction and `--delete` come from the plan, so
  `--resume` cannot be combined with them, with `--dry-run` or with the two
  locations. A resume runs the plan as it was analyzed; it does not analyze
  again, so run `bb sync` instead to see what changed since.
* A plan is resumed with the account that made it: with another account it is
  a usage error that names the right one (`--account ID`), and for the
  `environment` account the same `BB_*` variables must be set. Before any row
  runs, one request checks the prefix, so changed credentials or a deleted
  bucket are a single error (exit 3 or 2) and the plan stays as it was.
* A plan ID that does not exist is `not_found` (exit 2). Do not resume a plan
  that another program (the desktop application, or another `bb`) is
  executing.

### `bb presign S3_URI [--expires DURATION] [--method get|put] [--shorten]`

Prints a presigned URL (only the URL, so `curl "$(bb presign ...)"` works).
`--expires` defaults to `1h` and accepts seconds (`90`) or groups of a number
and a unit `s`, `m`, `h`, `d` (`30s`, `15m`, `1h30m`, `7d`), from 1 second
to 7 days; anything else is a usage error. `--method put` creates an upload
URL (any `Content-Type`, no checksum required).

`--shorten` also sends the URL to a URL shortener and prints the short link
instead. It is off by default and the whole URL, signature included, leaves
this machine, so `bb` says so on standard error. Choose the service in the
settings (the desktop app's Settings, or `provider = "bitly"` or `"tinyurl"`
under `[settings.shortener]` in `config.toml`); without one, `--shorten` is a
usage error and nothing is sent. The API token is the one stored by the
desktop app, or the `BB_SHORTENER_TOKEN` variable (use it when a master
password locks the stored one). Bitly needs a token, TinyURL works without.
`BB_SHORTENER_ENDPOINT` replaces the service's endpoint (a compatible service
of your own, or a local server in tests). A short link keeps working after the
presigned URL expires only as a redirect to a URL that no longer works. With
`--json` the record has an extra `short_url`.

### `bb edit S3_URI [--app APP]`

Edits one object in place: downloads it to a private copy under the data
folder (`data/edit/<uuid>/`, mode 0700 on Unix), opens the copy, and uploads
every save until Ctrl-C. Without `--app` the copy opens with the system's
default application (`open` on macOS, `xdg-open` on Linux, the file handler on
Windows); `--app` is an `.app` bundle on macOS (opened with `open -a`) or a
program elsewhere (run with the copy's path as its argument). `bb` does not
wait for the application to exit.

A save is uploaded one second after the file stops changing, whether the
editor writes in place or writes a new file and renames it (the watcher
covers both). The object's content type, `Cache-Control`,
`Content-Disposition`, `Content-Encoding`, user metadata, storage class and
tags are kept. Before each upload `bb` reads the object: if its ETag is no
longer the one `bb` last saw (somebody changed or deleted it), the save is
**not** uploaded and the object is never overwritten silently. On a terminal
`bb` asks: overwrite (`o`), save your version as a copy `name (edited).ext`
(`c`), discard your changes (`d`) or decide later (`l`). Without a terminal
(or with `--json`) it reports the conflict on standard error and keeps
watching; stopping then fails with exit code 2 (`conflict`) and the message
names the folder that keeps your edited copy. The upload also carries
`If-Match` where the server supports it (a server that answers `501` is not
asked again).

Ctrl-C ends the session: a save that is pending is uploaded first and the copy
is deleted (exit code 0). A conflict or a failed upload keeps the copy and
exits with the error's code. A second Ctrl-C stops waiting. Folders, a bucket
without a key, objects that do not exist and client-side encrypted objects
are rejected. A versioned bucket always edits the current version, and every
upload creates a new one.

Each event prints a line (`editing`, `uploaded`, `stopped editing`; problems
go to standard error); with `--json` one record per event:
`{ event, bucket, key, local_path, etag, copy_key, message }` with `event` one
of `started`, `uploaded`, `conflict`, `error`, `copy_saved`, `discarded`,
`stopped`. Copies of sessions that never ended are deleted at a later run once
they are a week old.

### `bb scan [--bucket B]`

Security scan (T13): reads the access settings of one bucket, or of every
bucket of the account, and prints the findings, most severe first (critical,
high, medium, low). It checks the bucket ACL for grants to everyone or to any
AWS account, whether the policy is public (`GetBucketPolicyStatus`, or a
conservative reading of the policy text on servers without it), the public
access block (off or partial), default encryption, versioning and object
ownership (ACLs enabled). Each finding names the bucket Properties tab that
fixes it. A check the server refuses or does not implement is printed as
`skipped`, an unreachable bucket as `error`; neither stops the scan. The scan
only reads, and findings are results: the exit code is 0 when the scan ran. The
summary goes to stderr. With `--json` each line is a record with `bucket`,
`kind` (`finding`, `skipped`, `error`), `severity`, `id`, `title`, `fix_tab` and
`explanation`.

### `bb mount BUCKET DIR [--prefix P]` (build with `--features mount`)

Mounts a bucket read-only as the existing folder `DIR` (Linux and macOS: FUSE; Windows:
Dokan, where `DIR` may also be a free drive letter such as `X:`) and keeps running
until Ctrl-C or until the mount is removed from outside (`umount`, `fusermount -u`,
ejecting the drive). The command exists
only in binaries built with `cargo build -p bucketbridge-cli --features mount`; other
builds do not list it. See docs/mount.md for installing macFUSE, libfuse or Dokan. Exit codes:
2 when the driver is missing or `DIR` is not a folder, 3 for access or listing failures.

### `bb mpu list [--bucket B] [--prefix P]` and `bb mpu abort --bucket B (--key K --upload-id ID | --all [--prefix P]) [--dry-run]`

`list` shows uncompleted multipart uploads of one bucket, or of every bucket
(on AWS each bucket is addressed in its own region). `abort` aborts one
upload, or with `--all` every upload of the bucket (under `--prefix`), which
frees the storage of their parts. `--dry-run` prints what would be aborted.
Some servers, MinIO among them, only list uploads for an exact object key:
pass the key as `--prefix` there.

### `bb export S3_URI --format csv|parquet|xlsx --out FILE [-r]`

Writes the listing of `s3://bucket` or `s3://bucket/prefix` to `FILE`. Without
`-r`, `--recursive` the rows are what `bb ls` shows for the prefix: folders
first (a folder row has only its key, which ends in `/`), then the objects of
that one level. With `-r` every object below the prefix is a row, in key
order. The folder marker of the prefix itself is never a row.

The columns are the same in every format: `key`, `size`, `last_modified` (UTC),
`etag` (without quotes) and `storage_class`; values the server did not send are
empty (CSV, XLSX) or null (Parquet).

| Format | File |
|---|---|
| `csv` | UTF-8, header row, RFC 3339 times. A text field that starts with `=`, `+`, `-`, `@`, a tab or a carriage return gets a leading `'`, so a spreadsheet program never runs an object key as a formula. |
| `parquet` | Snappy-compressed pages, one row group per 50,000 rows; `size` is `INT64`, `last_modified` a UTC `TIMESTAMP(MILLIS)`. |
| `xlsx` | Sheet `Objects` (`Objects 2` and so on beyond Excel's 1,048,576 rows): bold header, frozen header row, filter buttons, sizes as numbers, times as dates, text cells only. |

The listing is streamed: CSV never holds more than a page of rows, XLSX
(constant-memory writer) and Parquet (one row group) need bounded memory, so a
bucket of any size can be exported. The data goes to a temporary file next to
`FILE`, which is renamed over it when the export is complete: an error or
Ctrl-C leaves no partial file and an existing `FILE` untouched. A missing
folder for `FILE` is an `io` error (exit 2). A relative `FILE` is made absolute
against the working directory.
### `bb tag get|set|delete`, `bb storage-class`, `bb sse` and `bb acl get|set`

Object metadata (plan task T06). Each command takes an object (`s3://bucket/key`)
or, with `-r`, `--recursive`, a prefix ending in `/` (or a whole bucket): every
object under it, folder markers skipped, 200 keys at a time with up to 8 objects
in flight. A prefix needs `-r` and `-r` needs a prefix (usage errors). Every
object gets a line or record; a failed object never stops the others, and the exit
code is that of the first failure (`2 of 10 objects could not be changed`).
`--dry-run` reads but writes nothing and prints what would change. An object that
already is as wanted is `unchanged` and nothing is sent for it.

| Command | Effect |
|---|---|
| `bb tag get URI [-r]` | Prints `s3://bucket/key<TAB>KEY=VALUE`, one line per tag (nothing for an object without tags). |
| `bb tag set URI KEY=VALUE... [--replace] [-r] [--dry-run]` | Adds or overwrites the tags; other tags stay. `--replace` makes them the whole set. Tags follow the S3 rules (at most 10, keys of 1 to 128 and values of at most 256 letters, digits, spaces and `+ - = . _ : / @`, no `aws:` prefix); bad tags are usage errors. |
| `bb tag delete URI [KEY...] [-r] [--dry-run]` | Removes the named tags, or every tag when none is named. |
| `bb storage-class URI CLASS [-r] [--dry-run] [--key-file FILE]` | `CLASS` is `STANDARD`, `STANDARD_IA`, `ONEZONE_IA`, `GLACIER_IR`, `GLACIER`, `DEEP_ARCHIVE`, `INTELLIGENT_TIERING` or `REDUCED_REDUNDANCY` (any letter case). |
| `bb sse URI s3\|kms[:KEYID]\|c [--bucket-key] [--key-file FILE] [--source-key-file FILE] [-r] [--dry-run]` | `s3` is SSE-S3; `kms` uses the AWS managed key, `kms:KEYID` a key ID, ARN or alias, `--bucket-key` adds an S3 Bucket Key; `c` is SSE-C and needs `--key-file`, the new key. |
| `bb acl get URI [-r]` | Prints `s3://bucket/key<TAB>private` (or `public-read`, `public-read-write`, `authenticated-read`), or `custom` followed by the grants (`grantee:PERMISSION,...`). |
| `bb acl set URI CANNED [-r] [--dry-run]` | `CANNED` is `private`, `public-read`, `public-read-write`, `authenticated-read`, `bucket-owner-read` or `bucket-owner-full-control`. A bucket with ACLs disabled (Object Ownership BucketOwnerEnforced) is a `conflict` that says so. |

Changing the storage class, the encryption or (in the desktop app) the headers
copies the object onto itself, the only way S3 offers: content headers, user
metadata, tags, the storage class or encryption that was not changed, the website
redirect and the ACL are kept (the ACL is read first and applied again when it is
not the default one; a failure there is a warning), the object gets a new
modification time (and a new version on a versioned bucket), and objects above
5 GiB are copied in parts. An archived object (`GLACIER`, `DEEP_ARCHIVE`, an
Intelligent-Tiering archive tier) that is not restored fails with `conflict`
before any copy is sent. The source is pinned by ETag, so an object that is
overwritten meanwhile fails with `conflict` instead of being replaced.
Some S3-compatible servers (MinIO among them) refuse a copy onto itself that only
changes the storage class or the encryption; they accept tag and ACL changes.

SSE-C keys never appear on the command line: a key file holds 32 raw bytes, 64
hexadecimal digits or the base64 of 32 bytes (surrounding white space is
ignored), is read once and zeroed, and its content is never printed, logged or
included in an error. `--key-file` of `bb storage-class` and `--source-key-file`
of `bb sse` give the key of objects that are encrypted with SSE-C now; without it
S3 rejects the request (HTTP 400, `remote`, the message mentions SSE-C), and a
wrong key is refused (HTTP 403, exit 3 `auth`, the message says S3 refused the
customer-provided key), with nothing changed. A key for an object that is not
encrypted with one is rejected too. `bb sse URI c` needs `--key-file`;
`--key-file` with `s3` or `kms`, and `--bucket-key` without `kms`, are usage errors.

### `bb lock get|set` and `bb restore`

Object Lock and Glacier restore (plan task T07). Same batch rules as the object
metadata commands (`-r` for a prefix, one record per object, `--dry-run`).

| Command | Effect |
|---|---|
| `bb lock get URI [-r] [--version-id ID]` | Prints `s3://bucket/key<TAB>GOVERNANCE until 2099-01-02T03:04:05Z + legal hold` (or `none`). |
| `bb lock set URI [--mode GOVERNANCE\|COMPLIANCE --until DATE] [--clear-retention] [--legal-hold on\|off] [--bypass-governance] [-r] [--version-id ID] [--dry-run]` | Sets or clears the retention and the legal hold. `--until` is a date (`YYYY-MM-DD`, UTC) or an RFC 3339 time in the future. `--bypass-governance` sends `x-amz-bypass-governance-retention`. A refusal by the server is printed as the server worded it. |
| `bb restore URI --days N [--tier Expedited\|Standard\|Bulk] [-r] [--version-id ID] [--dry-run]` | Requests the restore of archived objects (`restore-version` is a different command). |

Records: `lock get` has `mode`, `retain_until`, `legal_hold`; `lock set` has `aspect` (`retention` or `legal_hold`), `mode`, `retain_until`, `legal_hold`, `previous`, `dry_run`, `changed`; `restore` has `days`, `tier`, `dry_run`, `requested`. All have `bucket`, `key`, `error`.

### `bb cloudfront`

Distributions and invalidations of the selected account (plan task T09). Only AWS
accounts have CloudFront: an account with a custom endpoint (`BB_ENDPOINT` or a
stored endpoint) fails with `usage` (exit 1) before anything is sent. Requests are
signed for `us-east-1` (`cn-northwest-1` for China accounts) whatever the account
region is.

| Command | Effect |
|---|---|
| `bb cloudfront list [--bucket B]` | Prints `ID<TAB>status<TAB>enabled\|disabled<TAB>domain<TAB>comment`, one line per distribution (all pages); `--bucket` keeps those with an origin in the bucket. |
| `bb cloudfront show ID` | Prints the details: ARN, domain, state, aliases, default root object, invalidations in progress and the origins (with the bucket each one serves). |
| `bb cloudfront create --bucket B [--region R] [--comment TEXT] [--default-root-object KEY] [--price-class CLASS] [--disabled]` | Creates a distribution with an origin access control for the bucket and prints the bucket policy statement to add. The bucket is not changed. `--region` defaults to the region the bucket listing reports. |
| `bb cloudfront enable ID` / `disable ID` | Turns the distribution on or off (the change is applied with the ETag just read). |
| `bb cloudfront detach ID --bucket B` | Removes the origins of the bucket from the distribution; refused with `conflict` when it would be left without an origin or a behaviour still uses one. Nothing is deleted. |
| `bb cloudfront invalidate ID PATH...` | Creates an invalidation. A path starts with `/`, `*` is allowed only as the last character, no blanks inside (use `%20`); paths are trimmed and repeats dropped. Bad paths are usage errors listing every problem, and nothing is sent. |
| `bb cloudfront invalidations ID` | The invalidation history, newest first (all pages): `ID<TAB>status<TAB>created`. |
| `bb cloudfront invalidation ID INVALIDATION_ID` | One invalidation with its paths. |

Records (`--json`) are the shapes of `docs/desktop-api.md`, "CloudFront (T09)":
`list`, `show`, `enable`, `disable` and `detach` print distributions (`id`, `arn`,
`domain_name`, `status`, `enabled`, `comment`, `aliases`, `price_class`,
`last_modified`, `origins`, and for `show`, `enable`, `disable` and `detach` also
`default_root_object`, `in_progress_invalidations`, `etag`); `invalidate`,
`invalidations` and `invalidation` print invalidations (`id`, `status`,
`create_time`, `paths`, empty in the history); `create` prints `distribution`,
`origin_access_control_id` and `bucket_policy` (a JSON document as a string). An
unknown distribution or invalidation is `not_found` (exit 2); a distribution that
changed meanwhile is `conflict` (exit 2). `--quiet` hides the outcome of `create`,
`enable`, `disable`, `detach` and `invalidate`.
### `bb iam`

Manages IAM users for AWS accounts (an account with a custom endpoint fails
with a usage error, exit 1). Commands that destroy something need `--yes`;
without it nothing is sent. Secrets are never command-line arguments.

| Command | Effect |
|---|---|
| `bb iam users [--path-prefix P]` | `name<TAB>path<TAB>created<TAB>arn`, one line per user. |
| `bb iam user show USER` | The user with its managed and inline policies, access keys, console password and groups; a part that cannot be read says so and does not hide the others. |
| `bb iam user create USER [--path P]` | Creates a user. |
| `bb iam user update USER [--new-name N] [--new-path P]` | Renames and/or moves a user. |
| `bb iam user delete USER --yes` | Removes the keys, inline and managed policies, console password and group memberships, then the user. A failed step is reported on stderr and the user stays (exit 2). |
| `bb iam keys list USER` | `key ID<TAB>active\|inactive<TAB>created` (IAM never shows secrets again). |
| `bb iam keys create USER` | Creates a key. The secret is printed once, with a warning on stderr; `--quiet` does not hide it. |
| `bb iam keys activate\|deactivate USER KEY_ID` | Enables or disables a key. |
| `bb iam keys delete USER KEY_ID --yes` | Deletes a key. |
| `bb iam policy list-managed [--scope local\|aws\|all]` | Managed policies that can be attached (default `local`). |
| `bb iam policy list USER` / `attach USER ARN` / `detach USER ARN` | The managed policies of a user; attach or detach one. |
| `bb iam inline list USER` / `get USER POLICY` | Inline policy names; one document, pretty-printed. |
| `bb iam inline put USER POLICY --file FILE` | Creates or replaces an inline policy (`-` reads stdin). |
| `bb iam inline delete USER POLICY --yes` | Deletes an inline policy. |
| `bb iam login-profile show USER` | Whether the user has a console password. |
| `bb iam login-profile create USER [--password-stdin \| --generate] [--reset-required]` | Sets a console password: first line of stdin, generated (24 characters with upper and lower case, digit and symbol; printed once with a warning), or asked on the terminal. |
| `bb iam login-profile update USER [--password-stdin \| --generate] [--reset-required \| --no-reset-required]` | Changes the password and/or the reset requirement. |
| `bb iam login-profile delete USER --yes` | Removes the console password. |
| `bb iam validate-policy FILE` / `format-policy FILE` | Check or pretty-print a user policy document (`-` is stdin) without an account. An invalid document prints each problem and exits 1. |

## Output

Human-readable text by default: data on stdout, failures on stderr as
`bb: error: ...` (and `bb: failed to ...` or `bb: cannot ...` for single
items of a batch).

With `--json`, stdout carries exactly one JSON object per line and per item.
Field names are stable; timestamps are RFC 3339 UTC strings, sizes byte
counts, absent values `null`.

| Command | Record |
|---|---|
| `ls` (buckets) | `Bucket` of `docs/desktop-api.md`: `name`, `region`, `creation_date`, `owner`, `kind` |
| `ls` (objects) | `ObjectEntry`: `key`, `size`, `etag`, `last_modified`, `storage_class`, `checksums`, `is_prefix` |
| `ls --versions` | `key`, `version_id` (`null` for folders), `is_latest`, `is_delete_marker`, `size`, `etag`, `last_modified`, `storage_class`, `is_prefix` |
| `mb`, `rb` | `bucket`, `region` (requested region, `null` = default; always `null` for `rb`) |
| `rm` | `bucket`, `key`, `version_id` (with `--versions` or `--version-id`), `dry_run`, `deleted`, `error` |
| `restore-version` | `bucket`, `key`, `restored_version_id`, `version_id` (the new current version, `null` when the server reports none), `etag`, `size` |
| `cp`, `mv` | `source`, `destination` (local paths or `s3://` URIs), `size`, `etag`, `dry_run`, `copied` (the upload, download or copy was made), `source_deleted` (`mv` only), `error` |
| `sync` | three kinds of record, told apart by `type`: `row`, `summary` and, with `--summary`, `report` (below) |
| `presign` | `bucket`, `key`, `method` (`get`/`put`), `expires_secs`, `expires_at`, `url`, `short_url` (only with `--shorten`) |
| `mpu list` | `bucket`, `key`, `upload_id`, `initiated`, `storage_class`, `initiator` |
| `mpu abort` | `bucket`, `key`, `upload_id`, `dry_run`, `aborted`, `error` |
| `export` | `source` (the `s3://` URI), `format` (`csv`, `parquet`, `xlsx`), `file` (absolute path), `recursive`, `rows` (without the header), `bytes` |
| `account add`, `account list` | `Account` of `docs/desktop-api.md` (`id`, `label`, `access_key_id`, `secret_ref`, `session_token_ref`, `region`, `endpoint`, `path_style`, `request_payer`, `mfa_serial`) plus `is_default`; secrets are never printed, only their references |
| `account remove`, `account default` | `id`, `label` |
| `iam users`, `iam user create\|update` | `IamUser` of `docs/desktop-api.md`: `name`, `arn`, `user_id`, `path`, `created`, `password_last_used` |
| `iam user show` | `UserDetails`: `user` and the sections `managed_policies`, `inline_policies`, `access_keys`, `login_profile`, `groups` |
| `iam user delete` | `user_name`, `steps` (`kind`, `target`, `ok`, `error`), `deleted` |
| `iam keys list` | `user`, `access_key_id`, `status`, `created` |
| `iam keys create` | `user_name`, `access_key_id`, `secret_access_key` (shown once), `created` |
| `iam keys activate\|deactivate\|delete` | `user`, `access_key_id`, `status` (`active`, `inactive`, `deleted`) |
| `iam policy list-managed\|list` | `name`, `arn`, `attachment_count`, `description` |
| `iam policy attach\|detach` | `user`, `policy_arn`, `attached` |
| `iam inline list`, `get`, `put\|delete` | `user`, `policy`; `get` adds `name`, `document`; `put\|delete` have `action` |
| `iam login-profile show` | `user`, `exists`, `created`, `password_reset_required` |
| `iam login-profile create\|update\|delete` | `user`, `action`, `password` (only a generated one, else `null`) |
| `iam validate-policy` | `file`, `valid`, `issues` (`path`, `message`) |
| `iam format-policy` | `file`, `document` |
| `tag`, `storage-class`, `sse`, `acl` | one record per object, see [Object metadata records](#object-metadata-records) |

`error` is `null` on success, otherwise `{"code": ..., "message": ...}` with
the codes of `docs/desktop-api.md`. A batch (`rm -r`, `cp -r`, `mv -r`,
`mpu abort --all`) keeps going after a failed item, prints a record for
every item (including entries that could not even be planned, such as an
unreadable file), and finishes with an error such as `2 of 10 files could
not be uploaded` (for `io` failures followed by the first one's cause).
With `--json` the final error, and any error that stops a command, is a
single `{"code", "message"}` record on stderr. Usage errors found while
parsing the command line are always plain text.

### `bb sync` options, jobs and S3 to S3 (v0.2.0)

```
bb sync [OPTIONS] LOCAL S3_URI
bb sync [OPTIONS] S3_SOURCE S3_URI          # between two S3 prefixes
bb sync [OPTIONS] --job NAME
bb sync --resume PLAN_ID
bb sync jobs list | show NAME | delete NAME --yes
```

**S3 to S3.** With two `s3://` locations the first is copied to the second on the server
(the same bucket or two buckets of the account), by the rules of `upload`
(`docs/sync.md`, "Bucket to bucket sync"). `--direction` does not apply (usage error), the
two prefixes must not contain each other, and `--delete` removes objects that exist only
in the destination. A folder named `jobs` as first argument must be written `./jobs`,
since `bb sync jobs` is the job command.

**Filters and exclusion rules.**

| Option | Effect |
|---|---|
| `--exclude RULE` | a gitignore-style rule (repeatable): `*.tmp`, `build/`, `!keep.tmp`, `logs/**/debug.log` |
| `--exclude-from FILE` | rules from a file, one per line; `#` comments and blank lines are skipped. A missing file is an error (exit 2) |
| `--include GLOB` | only paths that match one of the globs are synced (repeatable) |

The rules apply to both sides, so an excluded path is neither copied nor deleted
(`docs/sync.md`, "Exclusion rules"). A bad rule is a usage error that names the rule.

**Analysis options.**

| Option | Effect |
|---|---|
| `--keep-empty-folders` | also sync empty folders (folder markers in S3, empty folders locally) |
| `--skip-duplicates` | treat files as equal when size and checksum match, even if the times differ |
| `--no-cache` | do not use the local cache of remote listings (it is never used with `--delete`) |
| `--parallel N` | list N remote folders at the same time, 1 to 32 (default 4) |
| `--summary` | end with a report: counts and bytes per action, the duration and the failures |

**Saved jobs.** A job is a named setup stored in `config.toml`; the desktop app creates
them ("Save as job"), and `--save-job NAME` stores the setup of a command line (with the
account in use) before running it, replacing a job of that name.

* `bb sync --job NAME` runs the job (name in any letter case, or its ID) with the job's
  account; `--account` is only accepted when it names that account. The job's locations,
  direction and `--delete` cannot be combined with `--job`. `--dry-run`, `--summary`,
  `--exclude`, `--exclude-from`, `--include`, `--keep-empty-folders`, `--skip-duplicates`,
  `--no-cache` and `--parallel` are added to the job's options for this run (rules of the
  command line come after the job's). `bb sync --job` runs the plan like `bb sync` does,
  including `--resume`.
* `bb sync jobs list` prints one line per job (`NAME<TAB>DIRECTION<TAB>FROM -> TO`), `show
  NAME` prints all its settings, and `delete NAME --yes` deletes it (without `--yes` it is
  a usage error and nothing is deleted). With `--json` each job is one record with the
  fields of `SyncJob` (`docs/desktop-api.md`, "Sync jobs and sync options (v0.2.0)"). The
  job commands need no account and no credentials.

**Records.** A row of an S3 to S3 sync has `source` (the source object) and `remote` (the
destination), and `local` is `null`; `local` of the `summary` record is the source prefix.
Rows of folders (`--keep-empty-folders`) have a path that ends in `/`. With `--summary`
the human format ends with

```
summary:
  uploaded: 3 files, 1.5 MiB
  deleted in S3: 1 file
  already in sync: 120 files
  took 2.4 s
```

and `--json` ends with one more record after the `summary` record:
`{"type": "report", "plan_id", "direction", "duration_ms", "cancelled", "upload", "download", "copy",
"delete_local", "delete_remote", "equal", "unsyncable", "failed", "failures"}`, where each
kind has `files`, `bytes`, `failed` and `pending`, and `failures` lists up to 100 rows
with `rel_path` and `error`. With `--dry-run` the report counts what would be done as
`pending`.

### `bb sync` records

Human format (data on stdout, failures on stderr):

```
uploaded /home/me/photos/a.jpg to s3://bucket/backup/a.jpg
downloaded s3://bucket/backup/b.jpg to /home/me/photos/b.jpg
deleted s3://bucket/backup/old.jpg
deleted /home/me/photos/gone.jpg
3 copied, 1 deleted, 120 already in sync
```

With `--dry-run` the lines read `would upload ...`, `would download ...`,
`would delete ...` and the last line `would copy 3, delete 1; 120 already in
sync`. The last line adds `, N failed` when rows failed and, for an
interrupted run, `, N not done`. Failures are `bb: failed to upload SOURCE to
DESTINATION: REASON` (also `download` and `delete`) for a row that failed while
running, and `bb: cannot sync PATH: REASON` for a file the analysis could not
plan. `--quiet` hides everything of a run that went well; failures, the last
line of a run that did not, and the resume hints stay.

With `--json`, stdout has one `row` record per row that is copied, deleted or
failed (rows that are already equal have none), as the rows finish, and then
exactly one `summary` record, the last line. Field names are stable.

`row` record:

| Field | Meaning |
|---|---|
| `type` | `"row"` |
| `plan_id` | the stored plan that runs the row; `null` for a dry run and when nothing was left to execute |
| `path` | path below the folder and the prefix, with `/` separators |
| `action` | `copy`, `delete`, or `failed` (a file the analysis could not plan) |
| `target` | `local` or `remote`: the side a copy writes to or a delete removes from; `null` for `failed` |
| `local`, `remote` | the local file and the `s3://` URI the row is about; `null` for `failed` |
| `size` | bytes of the file that is copied (the source) or deleted; `null` when unknown or `failed` |
| `dry_run` | whether this was a `--dry-run` |
| `done` | whether the copy or deletion was made |
| `error` | `null`, or the reason as a string (a row failure has no error code, since the executor records only its text, as `error` of `SyncRow` in `docs/desktop-api.md`) |

`summary` record. Every row of the plan is counted in exactly one of `equal`,
`copied`, `deleted`, `failed` and `pending`; after a resume the counts cover
the whole plan.

| Field | Meaning |
|---|---|
| `type` | `"summary"` |
| `plan_id` | the plan left in the store, to continue with `--resume`; `null` when none is left (a dry run, nothing to do, or every row done) |
| `direction`, `delete` | `upload`, `download` or `bidirectional`, and whether `--delete` was given |
| `local`, `remote` | the absolute folder and the `s3://bucket/prefix/` |
| `dry_run`, `cancelled` | a dry run; interrupted before the end |
| `equal` | rows where both sides already agree |
| `copied`, `deleted` | rows copied, rows deleted |
| `failed` | rows that could not be synced, or failed while running |
| `pending` | rows not attempted (interrupted); with `--dry-run` the rows a real run would execute |

When rows could be tried again, stderr (not with `--json`) says `bb: N failed
while running; try again with: bb sync --resume PLAN_ID`.

### Object metadata records

One record per object, with the object's `bucket` and `key`. `error` is `null`, or
`{"code", "message"}` with the codes of `docs/desktop-api.md` (an SSE-C key is
never part of a record). The change records carry `dry_run`, `changed` (`false`
when the object already was as wanted, and when it failed) and `warnings`
(for example an ACL that could not be applied again after a copy).

| Command | Fields besides `bucket`, `key` and `error` |
|---|---|
| `tag get` | `tags` (an object; `null` when the read failed) |
| `tag set` | `tags` (the requested ones), `replace`, `dry_run`, `changed`, `warnings` |
| `tag delete` | `removed` (the keys; `null` means every tag), `dry_run`, `changed`, `warnings` |
| `storage-class` | `storage_class` (wanted), `previous`, `dry_run`, `changed`, `warnings` |
| `sse` | `encryption` (wanted: `{"type": "s3"}`, `{"type": "kms", "key_id", "bucket_key"}` or `{"type": "c", "key_md5"}`), `previous` (`none`, `s3`, `kms` or `c`), `dry_run`, `changed`, `warnings` |
| `acl get` | `acl` (`private`, `public-read`, ..., or `custom`; `null` when the read failed), `owner`, `grants` (`grantee_type`, `grantee`, `permission`) |
| `acl set` | `acl` (wanted), `previous`, `dry_run`, `changed` |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success (also when the reader of stdout goes away, as in `bb ls \| head`) |
| 1 | usage error (`usage`) |
| 2 | remote or local failure: `remote`, `not_found`, `conflict`, `io`, `internal` |
| 3 | authentication: `auth`, `locked`, `mfa_required` |
| 130 | interrupted with Ctrl-C (`cancelled`) |

When items of a batch fail, the exit code is that of the first failure (for
example 3 when the first failed delete was `AccessDenied`).

`bb sync` is the exception: it exits with 0 when every row is done or equal,
and with 2 when any row failed or could not be synced (after all the other
rows were processed), whatever the reason of the failed rows. The final error
is `2 of 10 files could not be synced` (code `remote`). Problems that stop the
whole command (wrong arguments, bad credentials, a missing bucket, an
unreadable plan store) keep their own codes: 1, 3 and 2.

## Interrupting and misbehaving servers

Ctrl-C during `export` stops the listing and removes the partial file (the
destination is never touched before the export is complete), then `bb` exits
with 130. Ctrl-C during `cp` or `mv` stops every transfer: unfinished multipart
uploads are aborted, partial downloads removed, and sources of `mv` that
were not transferred yet stay. `bb` waits up to 10 seconds for that clean-up
(a second Ctrl-C stops waiting) and exits with 130. `sync` stops its run the
same way and leaves the rows that did not finish pending in the stored plan:
`bb` prints `bb: interrupted; stopping. Resume with: bb sync --resume
PLAN_ID`, then the summary (with `--json`, a `summary` record with
`cancelled` true and the `plan_id`, and no hint), and exits with 130; during
the listing and the analysis it stops at once and nothing is stored. Other
commands cancel the request in flight and exit with 130 at once. Ctrl-C at a
password prompt ends `bb` as usual. If the reader of stdout goes away during
a transfer, the transfers stop the same way (a `sync` plan stays resumable)
and `bb` exits with 0.

A server that hands out a continuation token or version marker it already
sent (the same one again, or a cycle) ends the listing with a `remote` error
instead of paging forever.

## Testing

`crates/cli/tests/cli/` runs the real binary against a test server
(`TestS3`) or a scripted endpoint (`MockS3`) for behaviour the default test
server lacks; see `docs/testing.md`. Account tests run the library
in-process with an in-memory secret store, so no test touches the OS
keychain. MinIO-only tests run with
`BB_TEST_BACKEND=minio cargo test -p bucketbridge-cli --features minio`.
