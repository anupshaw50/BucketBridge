# Packaging and releases

How BucketBridge release artefacts are built, signed, checksummed, and
published, and how the licence
audit works. Day-to-day build and test commands are in `docs/development.md`.

## Artefacts

Every release contains these files. `<v>` is the version without the leading
`v` of the tag, for example `1.2.0`.

| File | Platform | Contents |
|---|---|---|
| `BucketBridge_<v>_windows_x64.msi` | Windows 10/11, x64 | Installer built with WiX by `tauri build` |
| `BucketBridge_<v>_windows_x64_portable.zip` | Windows 10/11, x64 | `BucketBridge.exe`, `LICENSE`, `THIRD_PARTY_LICENSES.html`; runs without installation |
| `BucketBridge_<v>_macos_universal.dmg` | macOS, Apple silicon and Intel | Universal app bundle (`--target universal-apple-darwin`) in a disk image |
| `BucketBridge_<v>_linux_x86_64.AppImage` | Linux, x86_64 | Self-contained AppImage |
| `BucketBridge_<v>_linux_amd64.deb` | Debian and Ubuntu, amd64 | Debian package |
| `BucketBridge_<v>_linux_x86_64.tar.gz` | Linux, x86_64 | `bucketbridge` executable, `LICENSE`, `THIRD_PARTY_LICENSES.html` |
| `bb_<v>_windows_x64.zip` | Windows, x64 | `bb.exe` command-line tool, `LICENSE`, `THIRD_PARTY_LICENSES.html` |
| `bb_<v>_macos_universal.tar.gz` | macOS, Apple silicon and Intel | Universal `bb` binary, `LICENSE`, `THIRD_PARTY_LICENSES.html` |
| `bb_<v>_linux_x86_64.tar.gz` | Linux, x86_64 | `bb` binary, `LICENSE`, `THIRD_PARTY_LICENSES.html` |
| `THIRD_PARTY_LICENSES.html` | all | Licences of the Rust crates compiled into the app and the CLI |
| `SHA256SUMS` | all | SHA-256 checksums of all the files above |

Each `.zip` and `.tar.gz` unpacks into a single folder named like the archive.
The desktop executables in the portable zip and the Linux tarball are the
ones `pnpm tauri build` leaves in `target/release/`, built with the UI
embedded; a plain `cargo build` of the desktop crate would be a development
binary that expects the Vite dev server, so the workflow never uses one.

The Linux artefacts are built on Ubuntu 24.04, the oldest Ubuntu image
GitHub-hosted runners still support, so they need glibc 2.39 or newer (for
example Ubuntu 24.04, Debian 13, or Fedora 40 and later) and WebKitGTK 4.1.
The `.deb` declares its library dependencies; the `.tar.gz` expects them to be
installed already, and the AppImage bundles most of them.

## Release process

1. Set the new version in `[workspace.package] version` in the root
   `Cargo.toml`, and in `crates/desktop/tauri.conf.json` if that file sets a
   `version`. Run `cargo update --workspace` so that `Cargo.lock` records the
   new version (every build uses `--locked`), and commit.
2. Tag the commit and push the tag:

   ```sh
   git tag -a v1.2.0 -m "BucketBridge 1.2.0"
   git push origin v1.2.0
   ```

3. The `Release` workflow (`.github/workflows/release.yml`) then:
   1. builds the Windows, macOS, and Linux artefacts in parallel, signing
      them where the secrets below exist;
   2. collects everything, writes `SHA256SUMS`, and checks it with
      `sha256sum --check`;
   3. creates a **draft** GitHub release for the tag with all files attached
      and generated release notes.
4. Review the draft on the Releases page, download and try the artefacts,
   then publish it by hand.

Version rules, enforced by `scripts/ci/release.mjs` before anything is built:

- the tag must be `v` followed by the workspace version;
- `bucketbridge-cli`, `bucketbridge-desktop`, and `tauri.conf.json` (if it
  sets a version) must agree;
- the version must be plain `MAJOR.MINOR.PATCH` and at most `255.255.65535`,
  because MSI product versions allow nothing else.

Re-running the workflow for the same tag replaces the assets of the existing
draft. A release that is already published is never modified: the run fails
instead.

### Dry run

Start the `Release` workflow by hand (Actions, Release, Run workflow) on any
branch. It runs the same CI, builds, and checksums, using the workspace
version for the file names, and stores the result as the `release-assets`
workflow artifact. It does not create a release or need write access.

## Signing and notarisation

Signing switches on per platform when the secrets exist (Settings, Secrets
and variables, Actions). Without them the release still succeeds and the
workflow log shows a warning.

### Windows

| Name | Kind | Value |
|---|---|---|
| `WINDOWS_CERTIFICATE` | secret | Base64 of a `.pfx` file with the code-signing certificate and its private key |
| `WINDOWS_CERTIFICATE_PASSWORD` | secret | Password of the `.pfx` file |
| `WINDOWS_TIMESTAMP_URL` | variable, optional | RFC 3161 timestamp server; defaults to `http://timestamp.digicert.com` |

With both secrets set, the workflow imports the certificate into the
runner's certificate store, Tauri signs the executable inside the MSI and the
MSI itself, and the workflow signs the `BucketBridge.exe` and `bb.exe` that go
into the zip archives, then checks them with `signtool verify /pa`. All
signatures use SHA-256 and a timestamp.

To create the secret value from a `.pfx` file:

```sh
base64 -w 0 codesign.pfx       # Linux
base64 -i codesign.pfx         # macOS
```

```powershell
[Convert]::ToBase64String([IO.File]::ReadAllBytes("codesign.pfx"))
```

This method needs a certificate whose private key can be exported to a
`.pfx` file. Publicly trusted code-signing certificates issued since June
2023 keep the key in a hardware token or a cloud signing service and cannot
be exported; using one of those needs a signing command for that service
(Tauri's `bundle.windows.signCommand`), which this workflow does not set up.

### macOS

| Name | Kind | Value |
|---|---|---|
| `APPLE_CERTIFICATE` | secret | Base64 of a `.p12` export of the "Developer ID Application" certificate and its private key |
| `APPLE_CERTIFICATE_PASSWORD` | secret | Password of the `.p12` file |
| `APPLE_ID` | secret | Apple ID used for notarisation, for example `developer@example.com` |
| `APPLE_PASSWORD` | secret | An app-specific password for that Apple ID |
| `APPLE_TEAM_ID` | secret | The 10-character team ID of the developer account |

Export the certificate from Keychain Access as a `.p12` file that contains
only the Developer ID Application identity, then encode it with
`base64 -i certificate.p12`.

| Secrets present | Result |
|---|---|
| none | App ad-hoc signed and not notarised. Users must allow it under System Settings, Privacy & Security, the first time it runs. |
| certificate and password | App and disk image signed with the Developer ID, not notarised |
| all five | Signed; Tauri notarises and staples the app, and the workflow notarises and staples the disk image |

The `bb` binary in `bb_<v>_macos_universal.tar.gz` is ad-hoc signed only; it
is not signed with the Developer ID or notarised. After downloading it with
a browser, clear the quarantine flag before the first run:
`xattr -d com.apple.quarantine bb`.

### Linux

Linux artefacts are not signed. Check them against `SHA256SUMS`.

## Verifying a download

Download `SHA256SUMS` into the folder that holds the downloaded files, then:

```sh
sha256sum --check --ignore-missing SHA256SUMS            # Linux
shasum -a 256 --check --ignore-missing SHA256SUMS        # macOS
```

On Windows, in PowerShell:

```powershell
Get-Content SHA256SUMS | ForEach-Object {
  $hash, $name = $_ -split '\s+', 2
  if (Test-Path -LiteralPath $name) {
    $actual = (Get-FileHash -LiteralPath $name -Algorithm SHA256).Hash
    '{0}: {1}' -f $name, $(if ($actual -eq $hash) { 'OK' } else { 'FAILED' })
  }
}
```

To check signatures:

- Windows: `Get-AuthenticodeSignature .\BucketBridge.exe` should report `Valid`.
- macOS: `xcrun stapler validate BucketBridge_<v>_macos_universal.dmg` for a
  notarised disk image, and
  `codesign --verify --deep --strict --verbose=2 /Applications/BucketBridge.app`
  for the installed app.

## Licence audit

Every dependency must be licence-audited. Run these three checks locally
(commands in `docs/development.md`) before every release:

| Check | Tool and policy | Fails when |
|---|---|---|
| cargo-deny | `cargo deny check advisories bans licenses sources` with `deny.toml` | a vulnerability advisory applies; a direct dependency is unmaintained, unsound, or yanked; a licence is not allowed; a dependency comes from outside crates.io; a version requirement is `*` |
| cargo-about | `cargo about generate` with `about.toml` and `about.hbs` | a crate's licence cannot be determined or is not accepted |
| npm licences | `scripts/ci/check-npm-licenses.mjs` on `pnpm --recursive licenses list --prod --json` | a production npm dependency has a licence outside the allowlist, or no usable licence information |

Allowed licences, identical in all three places: MIT, MIT-0, Apache-2.0
(also with the LLVM exception), BSD-2-Clause, BSD-3-Clause, ISC, Zlib, 0BSD,
BSL-1.0, Unicode-3.0, and MPL-2.0. MPL-2.0 is file-level copyleft; it is
needed by Tauri's HTML and CSS parser crates and by `option-ext`, which are
used unmodified. GPL, LGPL, and AGPL are not allowed.

The release workflow generates `THIRD_PARTY_LICENSES.html` with cargo-about,
puts it into every archive and attaches it to the release. It lists the Rust
crates of the whole workspace, excluding build-only and dev-only
dependencies. The licences of the npm packages bundled into the UI are
checked but not yet listed in a notice file.

When a check fails:

- **Vulnerability advisory:** update the crate (`cargo update -p <crate>`). If
  no fixed version exists and the advisory does not apply, add it to
  `[advisories] ignore` in `deny.toml` with a reason.
- **Yanked crate:** `cargo update -p <crate>`.
- **Licence not allowed:** replace the dependency. If it has to stay, add a
  crate-scoped entry to `[licenses] exceptions` in `deny.toml` (or to
  `EXCEPTIONS` in `scripts/ci/check-npm-licenses.mjs`) with a comment that
  justifies it. Adding a new licence to the allowlists is a project decision;
  change all three lists together.

## What the release workflow expects

- `crates/desktop/tauri.conf.json` exists, with the icons the bundles need in
  `bundle.icon` (as generated by `pnpm tauri icon`), and its
  `build.beforeBuildCommand` builds the UI (`pnpm --dir ui build` from the
  repository root); the workflow does not build the UI separately.
- `pnpm tauri <command>` at the repository root runs the Tauri CLI and finds
  `crates/desktop`. The workflow runs `pnpm tauri build --bundles msi` on
  Windows, `pnpm tauri build --target universal-apple-darwin --bundles dmg` on
  macOS (both Apple Rust targets installed), and
  `pnpm tauri build --bundles appimage,deb` on Linux. On Windows, signing
  settings are passed with `--config` only when the signing secrets exist.
- The `bb` binary comes from the `bucketbridge-cli` package.
- Packaging scripts: `scripts/ci/release.mjs` (version checks, picking the
  bundles), `scripts/ci/archive.mjs` (zip and tar.gz without external tools),
  and `scripts/ci/sha256sums.mjs`. They need only Node.js.
