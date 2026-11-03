# Security design

This document covers how BucketBridge stores secrets, protects them with an
optional master password, manages accounts, obtains MFA sessions and talks
through proxies. The code lives in
`bucketbridge-core`: `secrets` (store, vault, envelope crypto), `accounts`
(`AccountManager`), `session` (`Session`), `mfa` and `proxy`.

Acceptance criteria of T01 and where they are met:

| Criterion | Mechanism |
|---|---|
| Secrets never written to disk in plaintext | Secrets live only in the OS keychain (encrypted at rest by the OS) or, with a master password, as AES-256-GCM envelopes inside it. `config.toml` holds references, KDF parameters and a verifier only. |
| Wrong master password rejected without data loss | `Vault::unlock` and every password-changing operation verify the password before touching anything (the configuration, the keychain and the journal included, not a single write or delete); failures are `auth` and modify nothing. |
| Two accounts usable simultaneously | Secrets are addressed per account, MFA sessions are kept per account, and one thread-safe HTTP client serves every account. |

## Threat model

### Assets

- Secret access keys and static session tokens of every account.
- Temporary MFA session credentials.
- The proxy password.
- The master password and the key derived from it.

### What the OS keychain alone provides (no master password)

Each secret is a keychain item owned by the user's OS account. The OS encrypts
it at rest and releases it only to the logged-in user; on macOS other
applications additionally trigger a permission prompt. A copy of the disk
without the user's login secrets does not reveal it.

### What the master password adds

With a master password every stored value is an envelope encrypted under a key
derived from the password. This protects against:

- other software running as the same user that reads keychain items (on Linux
  and Windows any process of the user can usually read them);
- keychain exports, backups, synchronization and forensic copies;
- anyone who can use the unlocked desktop session while BucketBridge is locked.

Offline guessing of the master password from stolen envelopes or the verifier
costs one Argon2id evaluation (256 MiB, 3 passes) per guess.

### What it does not protect against

- Malware or a debugger running as the user while BucketBridge is unlocked:
  the key and decrypted secrets are in process memory while in use.
- A keylogger capturing the master password.
- A weak master password: the KDF slows guessing, it cannot make a guessable
  password strong. The UI should require a reasonable length.
- Tampering with `config.toml` (endpoints, proxy host, references, the
  security section) is limited, because its integrity is not protected but
  every use of it is checked:
  - A reference is only followed for the purpose it is stored for. An
    account's secret and session token must be owned by that account and sit
    in their own slot, the proxy password must be the proxy password of this
    installation. Any other reference (another account's secret, the secret
    key as a session token or as the proxy password) is rejected with a
    `usage` error before it is read, and is never re-encrypted or deleted.
    Editing the file therefore cannot make BucketBridge send an account
    secret key anywhere; SigV4 never sends the secret key itself.
  - Removing the master-password section while BucketBridge runs does not
    switch the vault to plain mode: the vault locks, announces it
    (`security://changed`) and refuses to unlock until the section is back
    (see Master password, States). A restart takes the file as it is.
  - What tampering can still do: make secrets unreadable (deleting the
    section also deletes the key derivation settings the secrets need: keep a
    backup of `config.toml`), and redirect requests. A redirected endpoint
    receives the signed requests of its account, including its static session
    token header and the MFA codes entered for it (an existing MFA session is
    dropped by an endpoint change, so credentials issued elsewhere are never
    sent there). A redirected proxy host receives the proxy password, which
    Basic and SOCKS5 authentication send in cleartext by protocol definition.
  - Without a master password, the OS keychain does not separate the
    configurations of one user: a program that edits the file can name the
    installation id and proxy reference of another configuration (both are
    readable in that configuration's file) and have its proxy password sent
    to a proxy host it chooses. With a master password every configuration
    has its own key, so another configuration's values do not decrypt.
- A byte-for-byte copy of `config.toml` is the same installation (it carries
  the installation id). Two copies that share one keychain share their items:
  rotating or removing in one deletes what the other still references. Use
  `--config-dir` / `BB_CONFIG_DIR` for separate installations, each starting
  from its own empty `config.toml`.
- Deletion of keychain items (denial of service).
- Proxy credentials on the wire: Basic authentication and SOCKS5
  username/password are cleartext to the proxy by protocol definition. Use a
  proxy on a trusted network.

## Guarantees

BucketBridge never writes to disk, logs or includes in error messages:

- secret access keys, session tokens (static or MFA), the proxy password;
- the master password or the derived key.

Supporting measures:

- In memory, secrets are `SecretString` values: zeroized on drop and redacted
  in `Debug`. Credentials, MFA sessions, the vault, pending changes, proxy
  credentials and HTTP clients all have redacting `Debug` implementations.
- The derived key is held in a zeroizing buffer; `lock` drops it (it is wiped
  as soon as the last in-flight read using it ends). The Argon2 working memory
  is wiped before it is freed, and the AES key schedule is wiped on drop.
- Keychain errors that carry raw bytes (`BadEncoding`, `BadDataFormat`) are
  wiped and replaced by messages that name only the reference.
- The SOCKS5 authentication message is built in an exactly sized, zeroizing
  buffer.

## Secret store

`SecretStore` is a key/value store of UTF-8 strings addressed by reference
(`get`, `set`, `delete`; deleting a missing value succeeds). All calls block
the calling thread, possibly while the OS shows a prompt; async code runs them
with `tokio::task::spawn_blocking`.

`KeyringStore` uses `keyring-core` with one store crate per OS. The `keyring`
crate itself (4.x) is only a sample front end that installs a process-wide
default store; its documentation directs applications to `keyring-core` and
the store crates, which is what is used here.

| OS | Backend | Item naming |
|---|---|---|
| macOS | Login Keychain, generic password items (`apple-native-keyring-store`, `keychain` feature) | service `BucketBridge`, account = reference |
| Windows | Credential Manager, generic credentials (`windows-native-keyring-store`) | target name `<reference>.BucketBridge`, persistence Enterprise (follows a roaming profile, like the config directory) |
| Linux, BSD | Secret Service over D-Bus: GNOME Keyring, KWallet (`zbus-secret-service-keyring-store`, `crypto-rust`) | attributes `service=BucketBridge`, `username=<reference>` |

Notes per backend:

- Linux uses the pure Rust zbus store: it needs no system library, encrypts
  the transport to the Secret Service, and generates its session key with the
  OS random generator. The libdbus based store generates that key with a
  non-cryptographic generator. Without a D-Bus session or a running Secret
  Service, `KeyringStore::new` fails with an `io` error that says so.
- KDE Wallet stores text only, which is why every stored value is a UTF-8
  string (envelopes are base64).
- Windows limits a credential to 2560 bytes. An envelope adds 30 bytes and
  base64 then grows it by a third, so a protected value can be at most 1890
  bytes (2560 without a master password); longer values fail with a `usage`
  error naming the limit.
- macOS may ask the user to allow access when the binary changes and is not
  signed with the same identity, for example in development builds.

`MemoryStore` keeps values in memory for tests.

Tests that touch the real keychain are `#[ignore]` and additionally require
`BB_TEST_KEYCHAIN=1`.

## Reference naming

Every value is addressed by `<owner>/<slot>/<generation>`:

- `owner`: the account id (a random UUID), or `proxy-<installation id>` for
  the proxy password (configurations from before installation ids used the
  fixed owner `proxy`, which every configuration on a machine shared);
- `slot`: `secret` (secret access key), `session-token`, or `password` (with
  owner `proxy`);
- `generation`: 1, 2, 3 and so on.

Example: `5f0c2a5e-0d6f-4a51-9d3e-2f6f1e0b9c11/secret/3`. A new value is always
written under the next generation of its slot, next to the old one, so the old
value stays readable until the configuration no longer references it.

Every configuration has a random installation id (`installation_id`, a UUID
v4 at the top of `config.toml`), created when the file is first written and
kept by every later write. Account ids are random, so account items never
collide between configurations; the proxy password lives under an owner
derived from the installation id, so two configuration folders on one machine
never share or overwrite it. A value for a slot that holds none yet never
overwrites an item that is already there: the vault refuses (`conflict`), and
the account manager takes the next free generation instead.

References are checked against their use (see What it does not protect
against). The checks apply before anything is read, re-encrypted or deleted:
reading credentials, the proxy password, rotation, removal and master password
changes. A master password change refuses to run while a reference that does
not belong to what it is stored for exists, so no secret is left out of it
unnoticed; entering the secret again (or removing the account) repairs it.
What an edited reference stopped pointing at stays in the keychain, since it
cannot be attributed.

Configurations written before installation ids keep working: the legacy
proxy reference `proxy/password/<n>` is still read. The proxy password moves
to the installation's own reference the next time the session opens or the
secrets are unlocked (`AccountManager::migrate_legacy_references`), with the
same two-phase protocol as any change: the new value is written first, the
configuration is saved, then the legacy value is deleted. Two legacy
configurations that already shared the old name were overwriting each other's
value; after the move each has its own, and one that finds its value gone
needs the proxy password entered again.

## Master password

### Key derivation

`key = Argon2id(password, salt)`, Argon2 version 0x13, 256-bit output.

| Parameter | New passwords | Accepted when loading |
|---|---|---|
| memory | 256 MiB (`m_cost_kib = 262144`) | 8 KiB per lane to 2 GiB |
| passes | 3 | 1 to 64 |
| lanes | 4 | 1 to 64 |
| salt | 16 random bytes | 16 to 64 bytes |
| password | 1 to 1024 bytes, used as typed (no Unicode normalization) | same |

The cost follows the procedure of RFC 9106, section 4: as much memory and time
as one call can afford. Unlocking happens once per session, so about a third
of a second is acceptable (0.36 s on one core of a 2020 ARM64 laptop in a
release build; the lanes run sequentially). The RFC's option for
memory-constrained environments (64 MiB, 3 passes, 4 lanes) takes only 85 ms
there. The parameters are stored in `KdfParams`; raising the default later
affects new and changed passwords only, and old data stays readable. Changing
the password to itself re-derives with the current default. The upper bounds
stop a damaged or hostile `config.toml` from making an unlock allocate
unbounded memory.

### Envelope format (version 1)

An envelope is standard base64 (with padding) of:

```text
0xBB | 0x01 | nonce (12 bytes) | AES-256-GCM ciphertext | tag (16 bytes)
```

- `0xBB` marks a vault envelope, `0x01` is the format version.
- The nonce is 96 random bits from the OS generator, fresh for every
  encryption.
- The associated data is
  `"BucketBridge vault" 0x00 | 0xBB | 0x01 | purpose | 0x00 | context`:
  - purpose `secret`, context = the secret's reference: a ciphertext copied
    to another reference does not decrypt;
  - purpose `verifier`, context = the canonical KDF parameters
    (`argon2id;m=...;t=...;p=...;salt=...`): modified parameters fail
    verification.
- The header is part of the associated data, so the version cannot be
  changed without detection. A different version is reported as unsupported.

Opening fails with a specific error for: not base64, truncated, not an
envelope, unsupported version, integrity check failed (modified data, wrong
reference, other master password) and not UTF-8.

### Verifier

`SecurityConfig { kdf, verifier_b64 }` is persisted in `config.toml`. The
verifier is an envelope of a fixed constant with purpose `verifier`. A
password is correct when the derived key opens it; nothing else about the
password or the key is stored.

### States

| `SecurityConfig` | key in memory | State | Reading a secret |
|---|---|---|---|
| absent | none | no master password | stored value as-is |
| present | none | locked | `locked` error |
| present | present | unlocked | envelope opened with the key |

`unlock` derives and verifies the key without touching the keychain. A wrong
password is an `auth` error and changes nothing; an unlocked vault stays
unlocked. `lock` drops the key. Removing secrets works while locked; writing
needs the key.

A protected vault never becomes a plain one on its own. When another process
changes the master password, the vault adopts the new settings and locks. When
the persisted configuration loses its master-password section while the vault
has one (a deleted section, an edited file), the vault locks and does not
adopt it: `status` stays `master_password_set: true, unlocked: false`,
`unlock` fails with `conflict`, and nothing can be stored (so no secret is
written without encryption). It recovers when the section is back, when the
master password is changed or removed through this vault (it verifies the
current password against the settings the vault still holds), or after a
restart, which takes the file as it is. Removing the master password in
another process is therefore only picked up by restarting.

## Crash-safe changes

Setting, changing and removing the master password, adding an account,
rotating keys, replacing or clearing a session token and setting the proxy
password all use the same two-phase protocol, because the configuration file
belongs to the account manager:

1. **Prepare** (`Vault::prepare_set_master_password`,
   `prepare_change_master_password`, `prepare_remove_master_password`,
   `prepare_secret_changes`): writes every new value under its new reference
   (the next generation). Old values are untouched. A wrong current password
   fails before anything is written. If a write fails, the new values written
   so far are deleted and the error is returned.
2. **Persist** (caller): atomically write `config.toml` with the new
   `SecurityConfig` (`PendingChange::security_update`) and the new references
   (`PendingChange::new_reference`).
3. **Finalize** (`PendingChange::finalize`): the vault switches to the new
   configuration and key, then deletes the obsolete values. If persisting
   failed, `PendingChange::abort` deletes the new values instead.

| Interruption | Persisted configuration | Its values | Leftovers |
|---|---|---|---|
| during prepare | old | intact | some new values |
| after prepare, before persisting | old | intact | all new values (`written`) |
| after persisting, before finalize | new | intact | all old values (`obsolete`) |
| deletion fails in finalize | new | intact | the failed ones (`Cleanup::leftover`) |

Leftovers are never referenced by any configuration. They are ciphertext,
except after an interrupted removal of the master password, where the new
values are plaintext copies inside the OS keychain, the same protection as
without a master password. The account manager journals and removes them
(next section).

The journal is made durable before the first value is written: written to a
temporary file, flushed, renamed into place, and the directory is flushed. A
journal that cannot be read is not discarded: it is moved aside
(`secrets-journal.json.damaged-<n>`), the damage is logged and returned as an
`io` error before anything is written, because the leftovers it listed
(plaintext copies after an interrupted removal of the master password, for
example) can no longer be found automatically; the next attempt proceeds.

Only one change can be pending per vault; another `prepare` fails with
`conflict`. Dropping a pending change releases that slot but deletes nothing,
since the vault cannot know whether it was persisted. A `lock` between prepare
and finalize is honoured: the vault stays locked. `Vault::sync_config` adopts
a configuration that another process changed (the vault then locks); it does
nothing while a change of its own is pending.

## Account manager

`AccountManager` (blocking API) ties `config.toml` to the secret store through
the vault. It is the only writer of secret references, and it writes the
configuration only through `ConfigStore::update` (an atomic, locked
read-modify-write).

| Operation | Secrets |
|---|---|
| `add` | writes the secret (and session token) as generation 1; the first account becomes the default |
| `update` | a new secret or session token is written as the next generation and the old value deleted (key rotation); `clear_session_token` deletes it |
| `remove` | removes the account from the configuration first, then deletes its secrets; works while locked; missing secrets are fine |
| `set_master_password` | re-encrypts every referenced secret: account secrets, session tokens and the proxy password |
| `set_proxy_password` | writes or deletes the proxy password (under `proxy-<installation id>`) and points `settings.proxy.password_ref` at it; a legacy reference is replaced and its value deleted, an edited one is only replaced or cleared |
| `migrate_legacy_references` | moves a legacy proxy password to the installation's own reference (session open, unlock) |
| `save_settings` | validates the settings including `proxy::validate_proxy_settings` (a host with `@` is rejected, and never echoed), and keeps the stored `password_ref`, so a caller cannot point the proxy at another secret |

Input checks: labels are unique case-insensitively (`conflict`); access key,
secret, region (`client::validate_region`), endpoint
(`client::validate_endpoint`) and MFA serial are validated before anything is
stored. Secrets are stored exactly as given; control characters (a pasted
line break) are rejected. New account ids are random UUID v4s.

Every secret-changing operation holds an in-process lock and the advisory
file lock `secrets.lock` in the config directory for its whole duration, so
the desktop app and the CLI never interleave changes, and readers never see a
half-applied change. It then:

0. verifies the current master password when a master password change is
   asked for, before anything below happens;
1. records every reference it will write or make obsolete in
   `secrets-journal.json` (references only, never secrets);
2. prepares the vault change;
3. persists the configuration; when `ConfigStore::update` reports an error
   after possibly writing, the file is read back to decide;
4. finalizes the vault change;
5. purges: deletes every journal reference that the persisted configuration
   does not reference.

The persisted configuration is the single source of truth, so a crash at any
point leaves it referencing values that exist, and the journal lists exactly
the values that may be left over. `Session` purges them at startup. Purging
runs under the same file lock, so it never deletes a value another process
has written but not yet persisted.

## Session

`Session` is the process-wide entry point the desktop app and the CLI hold. It
owns the directories, the account manager, the in-memory MFA sessions, one
proxy-aware HTTP client and one S3 handle per account. Its async methods run
keychain, key-derivation and file work on blocking threads.

- `s3_for(account_id)` fails with `locked` while a master password is set and
  locked, then with `mfa_required` when the account has an MFA serial but no
  valid MFA session; otherwise it returns a cached handle, built with
  `ClientOptions::from_settings` and the proxy-aware client, signed with the
  MFA session's temporary credentials when there is one.
- A cached handle is reused only while everything it was built from is
  unchanged: the account (including its secret references, so a rotation
  is noticed, also when another process made it), the client settings, the
  HTTP client and the MFA session. The HTTP client is rebuilt when the proxy
  settings or the proxy password change.
- `lock` drops the key, every MFA session and every cached client (they hold
  credentials). Rotating an account's keys, changing its MFA serial or
  endpoint, or removing it drops its MFA session.
- `unlock`, `lock` and master-password changes are broadcast as
  `SecurityStatus` (`subscribe`); the desktop forwards them as
  `security://changed`. A change found while reading `config.toml` is
  broadcast too (another process changed the master password, or its section
  disappeared), and locks the session like `lock` does.
- Without a usable OS keychain (for example Linux without a Secret Service)
  the session still opens; everything that needs a stored secret fails with
  an `io` error that names the reason.

## MFA sessions

1. `mfa_login` passes the account's long-term keys, its `mfa_serial` and the
   six digit code to STS `GetSessionToken` (`mfa::login`).
2. The temporary credentials are kept in memory only (`MfaSessions`), per
   account, until their expiry minus 5 minutes. They are never written
   anywhere.
3. Account-bound operations take credentials from
   `MfaSessions::credentials`, which fails with `mfa_required` when the
   account has no session or the session is within the margin of its expiry.

Details:

- Input is checked before STS is called: a code is exactly six ASCII digits,
  a serial is 9 to 256 characters of the STS alphabet, a duration is whole
  seconds from 900 to 129600 (`None` lets STS choose: 12 hours for IAM users,
  1 hour for root credentials).
- Retries are disabled: a code is meant to be used once, and a retried request
  could be rejected for reusing it. The call times out after 30 seconds.
- STS failures go through the core's `SdkError` conversion: a rejected code
  or rejected keys (`AccessDenied`, `InvalidClientTokenId`, and so on) are
  `auth` errors; other STS and network failures are `remote`.
- A session that is already within the margin when it arrives (a wrong
  system clock) is not stored and the login fails with a message about the
  clock.
- A session is bound to what it was issued for: the account's access key id,
  secret and session token references, endpoint and MFA serial.
  `MfaSessions::credentials` drops a session whose account no longer matches
  (`mfa_required`), so temporary credentials issued for one server are never
  sent to another, and a rotation or a new MFA device made by another process
  is noticed on the next request.
- A login that is waiting for STS stores nothing if, meanwhile, the secrets
  were locked (`MfaSessions::clear`) or the account's session was removed
  (rotation, new MFA serial, endpoint change, removal): `generation()` is read
  before the credentials are read, `insert_since` stores only if nothing it
  depends on was forgotten since, and `Session::mfa_login` re-reads the
  account after STS answers (`locked`, `not_found` or `conflict`). The next
  request asks for a new code.
- The STS client uses only the account's static credentials, never the
  environment, profiles or instance metadata.

STS endpoint:

- AWS accounts (no custom endpoint): the regional STS endpoint of the
  account's region as resolved by the SDK (`https://sts.<region>.amazonaws.com`
  or the China and GovCloud equivalents). Regional tokens are valid in every
  region.
- Accounts with a custom endpoint: the account's own endpoint URL, signed for
  the account's region. S3-compatible servers that implement STS serve it
  there. The credentials of such an account are never sent to AWS.

## Proxy

`proxy::build_http_client(settings, password)` returns one `SharedHttpClient`
for every SDK client (S3 and STS), so all accounts share its connection pools.
The proxy password is read from the vault through `settings.proxy.password_ref`.
While the proxy settings are disabled, the standard proxy environment
variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`) apply, exactly
as for the SDK's own default client; enabled settings always take precedence
(`HttpClientBuilder::environment_proxy(false)` turns the variables off).

| Route | Implementation |
|---|---|
| proxy disabled | the SDK's hyper 1.x client: rustls with aws-lc-rs, OS trust roots, pooling, environment proxy variables |
| HTTP proxy | the SDK's built-in proxy support (`aws_smithy_http_client::proxy::ProxyConfig`): https endpoints through `CONNECT` tunnels with TLS end to end, http endpoints through absolute-form forwarding; Basic authentication |
| SOCKS5 proxy | the SDK has no SOCKS support and no public transport hook, so the same parts are assembled: hyper-util's pooled client, rustls with aws-lc-rs and the SDK's cipher suites, the same trust roots, ALPN `h2` and `http/1.1`, over a SOCKS5 tunnel (RFC 1928) with optional username/password (RFC 1929) |

Behaviour:

- SOCKS5 sends host names to the proxy unresolved (like `socks5h`), so no DNS
  query leaves the machine directly. The default port follows the scheme (80,
  443).
- Credentials are percent-encoded when the SDK embeds them in its proxy URL,
  so every character survives. A self-check at build time confirms that the
  resulting configuration proxies every request with exactly those
  credentials; otherwise the build fails instead of silently connecting
  directly.
- Rejected proxy credentials (HTTP 407 or a SOCKS5 refusal) become a
  non-retryable dispatch failure. The core's `SdkError` conversion recognizes
  them (`proxy::is_proxy_auth_failure`) and reports them as `auth`, for S3
  and STS alike.
- On the SOCKS5 route the connect timeout covers the proxy connection, the
  handshake and TLS; the read timeout covers waiting for the response, as in
  the SDK's client. TLS and SOCKS protocol failures are not retried; network
  failures are.
- `proxy::HttpClientBuilder` can add trusted root certificates (for example
  the private CA of an S3-compatible server) or turn off the OS roots; the
  same roots apply to every route.
- `proxy::validate_proxy_settings` checks host, port, user name and bypass
  entries for the settings page; `AccountManager::save_settings` runs it too.
  An HTTP proxy user name cannot contain `:`; SOCKS5 user names and passwords
  are limited to 255 bytes. The host must not contain `@` (user information
  belongs in the user name and password fields), and a rejected host is never
  echoed in an error: it may be a password typed into the wrong field.

Bypass list (`settings.proxy.bypass`), matched case-insensitively against the
host of each request URL:

| Entry | Matches |
|---|---|
| `s3.example.com` | exactly that host |
| `*.example.com` or `.example.com` | any subdomain, not `example.com` itself |
| `10.1.2.3`, `::1`, `[::1]` | that IP address in the URL |
| `10.0.0.0/8`, `fd00::/8` | IP addresses in the range |
| `localhost` | `localhost`, `*.localhost`, `127.0.0.0/8` and `::1` |
| `*` | every host |

IP and range entries match only URLs that contain an IP address: host names
are never resolved to choose a route. Loopback addresses are not bypassed
implicitly, because through an SSH-forwarded SOCKS proxy they mean the remote
machine; add `localhost` to reach local servers directly. Ports, URLs and
other wildcards are rejected as `usage` errors.

## Client-side encryption envelope (T07)

Header (57 bytes): `BBCSE`, version 1, compression (0 none, 1 gzip, 2 zstd), KDF 1
(Argon2id), m/t/p costs (default 64 MiB, 3, 4), chunk size exponent (default 20),
16 byte salt, 4 byte nonce prefix, 16 byte key check. Body: chunks of
`u32 length || AES-256-GCM(plaintext chunk)`. The nonce is the prefix plus the
chunk index; the associated data is `"BBCSE chunk" || header || index || final`, so
reordering, repeating, dropping or truncating chunks and editing the header fail
authentication. The key check (a tag over an empty message) separates a wrong
password from damaged data. The marker metadata holds no secret. See
`crates/core/src/cse/format.rs`.

## Credential sources (v0.2.0)

An account's credentials come from its `source` (`model::CredentialSource`).
`crates/core/src/creds` builds a refreshing credentials provider for every
source except static keys, and the SDK clients of the account (S3, IAM,
CloudFront) are built on that provider (`build_client_with_provider`) instead
of fixed keys. The SDK asks the provider for credentials, keeps them until
shortly before they expire and asks again. Static-key accounts are built
exactly as before.

| Source | Provider | What is stored, and where |
|---|---|---|
| `static_keys` | fixed credentials read from the secret store | access key id in `config.toml`; secret key and optional session token in the secret store (see [Secret store](#secret-store)) |
| `aws_profile` | `aws-config` profile provider on the AWS files | the profile name in `config.toml`; keys stay in the AWS files, which BucketBridge never writes |
| `assume_role` | `AssumeRoleCredentials`: STS `AssumeRole`, signed through the source account | role ARN, session name, duration, external id and MFA serial in `config.toml` (none is a secret); the session lives in memory |
| `sso` | `SsoCredentials`: `GetRoleCredentials` with the cached portal token | start URL, region, account id, role name in `config.toml`; the portal token only in the AWS CLI cache file |
| `instance_role` | `aws-config` IMDSv2 or container credentials provider | nothing; the credentials live in memory |

Rules:

- **No secret in `config.toml`.** The only secret-bearing field of an account
  is `secret_ref`/`session_token_ref`, and accounts of other sources have none
  (`secret_ref` is empty). The secret-store bookkeeping (journal, master
  password re-encryption, rotation, purge) skips them. External ids are not
  secrets (they only stop a confused deputy) and are stored.
- **Order of failures in `Session::s3_for` is unchanged:** `locked`, then
  `mfa_required`, then `not_found`. A role through an MFA-protected source
  account asks for the source account's code too.
- **Credentials are never logged.** The SDK `Credentials` type redacts itself
  in `Debug`; the SSO token (`SsoToken`), the pending sign-in (`DeviceLogin`)
  and the providers print no token, client secret or device code. Provider
  failures keep their cause in the message ("not signed in ...", "the SSO
  session expired: sign in again", "no instance or container role ...") and are
  classified `auth`.
- **Assumed roles.** Session name (2 to 64 of `A-Za-z0-9_+=,.@-`), duration
  (900 to 43200 seconds), external id and role ARN are validated when the
  account is saved. Without an MFA device the provider assumes the role again
  once less than 5 minutes of the session is left. With one, the role's trust
  policy needs a fresh code: `mfa_login` calls `AssumeRole` with the serial and
  code and keeps the session like any MFA session (in memory, dropped on lock,
  on a change of the account or of its source, and when it expires, after which
  the account reports `mfa_required` again). A chain of roles is followed up to
  5 deep; loops are refused when the account is saved and again when the client
  is built. A source account cannot be removed while a role uses it. The STS
  call is not retried when it carries an MFA code.
- **AWS shared config.** The files are read with the official `aws-config`
  parser: `~/.aws/config` and `~/.aws/credentials`, or `AWS_CONFIG_FILE` and
  `AWS_SHARED_CREDENTIALS_FILE` (a leading `~/` is expanded). Importing creates
  accounts that hold only the profile name. Listing never returns a key. The
  profile provider supports what the AWS CLI does for a profile: static keys,
  `role_arn` with `source_profile` or `credential_source`, `credential_process`
  (the command runs with the user's own rights, as in the CLI), web identity and
  IAM Identity Center profiles.
- **Instance and container roles.** The container endpoint is used when
  `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` or `AWS_CONTAINER_CREDENTIALS_FULL_URI`
  is set, otherwise the instance metadata service with an IMDSv2 session token.
  Nothing is stored. Requests to the metadata service go through the
  application's HTTP client; the probe is bounded to 8 seconds.
- **IAM Identity Center (SSO).**
  - Sign-in is the OIDC device authorization flow: `RegisterClient` (public
    client, scope `sso:account:access`, grant types device code and refresh
    token), `StartDeviceAuthorization`, then `CreateToken` polls. The device
    code and the client secret live in memory only, in a pending sign-in that is
    forgotten after 20 minutes. `slow_down` lengthens the polling interval by 5
    seconds.
  - **Token cache shared with the AWS CLI.** The token is stored in
    `~/.aws/sso/cache/<sha1>.json` in the AWS CLI's format (`startUrl`,
    `region`, `accessToken`, `expiresAt`, `refreshToken`, `clientId`,
    `clientSecret`, `registrationExpiresAt`). `<sha1>` is the hex SHA-1 of the
    `sso-session` name when the profile or account has one, and of the start URL
    otherwise, which are the AWS CLI's rules, so `aws sso login` and BucketBridge
    use each other's sign-ins. The older `...UTC` timestamp form is read. Fields
    BucketBridge does not know are kept when it rewrites a file.
  - **The cache file is the only place a token is stored**: never in
    `config.toml`, the keychain, a log or an IPC message. It is created with mode
    0600 in a directory of mode 0700 (the directory is tightened when it lets
    others in), written to a random temporary name opened with `create_new` and
    renamed over the target, so a partial file is never seen. Signing out deletes
    the file. On Windows the default access rights of the user profile apply.
  - An access token is renewed with the refresh token 60 seconds before it
    expires (when the sign-in issued one and the client registration is still
    valid); otherwise the account fails with `auth` and the message asks to sign
    in again. Role credentials are requested again 5 minutes before they expire.
  - The SSO region decides the partition; GovCloud (`us-gov-*`) and China
    (`cn-*`) regions use their own OIDC and portal endpoints through the SDK's
    endpoint resolution.
- **Dependencies.** `aws-config` (features `default-https-client`, `rt-tokio`,
  `credentials-process`, `sso`), `aws-sdk-sso` and `aws-sdk-ssooidc` use the same
  rustls/hyper 1 stack as the other SDK crates (`default-features = false`);
  `cargo tree` finds no OpenSSL, native-tls or hyper 0.14.
