# Mount as drive (T04)

BucketBridge can show a bucket as a read-only folder through FUSE (Linux, macOS) or as a
read-only drive letter or folder through Dokan (Windows). It is an optional, non-default
cargo feature called `mount`. Default builds never compile the adapters.

## What it does

- Prefixes are folders, objects are files. Nothing can be created, changed or deleted
  (the mount is read-only and `noexec` on Unix, write-protected on Windows).
- A folder is listed once and kept for 30 seconds; names are looked up in that listing.
- File reads fetch aligned 1 MiB blocks with `Range` requests that send `If-Match` of
  the listed ETag, and keep at most 64 blocks in memory. A file that changed on the
  server fails the read with an I/O error instead of returning mixed content; the next
  access lists the folder again.
- If a folder and an object share a name, the folder wins. Names that cannot be file
  names are skipped. Object metadata, versions and storage classes are not exposed.
- Archived (Glacier) objects fail to read with an I/O error until restored.
- Windows only: names Windows cannot show are also left out (reserved characters
  `< > : " | ? *`, a trailing dot or space, device names such as `CON`, names over 255
  characters). They stay reachable with `bb cp` and in the browser.

## Design

`crates/core/src/mount.rs` is the filesystem-independent layer (inode table, listings,
ranged reads, block cache). It is always compiled and tested without a kernel
(`tests/tests/mount.rs`). `crates/core/src/mount/fuse.rs` is the thin adapter on the
`fuser` crate (MIT); `crates/core/src/mount/windows.rs` is the thin adapter on the
`dokan` crate (MIT). Each exists only with `--features mount` on its OS; the module
`mount::backend` names the one of the running platform, so the desktop host and the CLI
share their code. `crates/core/src/mount/target.rs` holds the Windows-independent logic
(drive-letter parsing, mount targets, path splitting, file-name and attribute mapping)
and is unit-tested on every platform.

With the default features of `fuser` turned off, Linux needs no libfuse headers (it uses
`fusermount`); macOS links macFUSE through `pkg-config`.

## Windows backend

Candidates, checked against `deny.toml`:

| Backend | Crates | Licences | Verdict |
|---|---|---|---|
| Dokan 2 | `dokan`, `dokan-sys` (+ `widestring`, `winapi`, `bitflags`, `libc`, `cc`) | MIT, MIT/Apache | chosen |
| WinFsp | `winfsp`, `winfsp-sys` | GPL-3.0 | rejected: copyleft, would need a `deny.toml` exception |
| ProjFS | `windows` crate | MIT/Apache | rejected: it projects placeholders and copies content onto the local disk when a file is opened, so it is not a streaming read-only view and leaves data behind |

Dokan gives a real virtual volume with ranged reads, which matches the core layer
one to one, and a drive letter. All crates in the tree are permissive, so
`cargo deny check` passes with no new exception. The Dokan driver and `dokan2.dll`
(a separate, user-installed component, like macFUSE) are not linked statically or
shipped by BucketBridge.

Runtime check (`mount::fuse_installed()`, reported as `fuse_installed` by `mount_status`):
`%SystemRoot%\System32\dokan2.dll` and `%SystemRoot%\System32\drivers\dokan2.sys`
exist. The mount dialog explains what to install when they do not.

Mount point: a free drive letter (`X:`, `x`, `X:\` all work; letters `C` to `Z` that are
unused are offered by `mount_status().drive_letters`), or an existing empty folder on an
NTFS volume. The volume is mounted write-protected and case sensitive, with a 60 second
request timeout (Dokan unmounts a volume whose requests time out). Unmounting happens on
`unmount_bucket`, when the app exits (the registry drops every mount) and on Ctrl-C in
`bb mount`; ejecting the drive from outside ends the mount and it disappears from the
list.

Known limits of the Windows adapter: it needs the Dokan 2 runtime at program start when
built with `--features mount` (the import library is linked normally), `dokan-sys` 0.3.1
declares `extern "stdcall"` functions that current rustc warns about on x86_64 (a
future-compatibility lint, not an error today), and drive letters are probed with
`Path::exists`, so a disconnected network drive can slow the first `mount_status`.

## Build

Linux:

```bash
sudo apt install fuse3            # or: dnf install fuse3; provides fusermount3 and /dev/fuse
cargo build --release -p bucketbridge-cli --features mount
pnpm tauri build --features mount # desktop app with the Mount as drive entry
```

macOS:

```bash
brew install --cask macfuse       # or install FUSE-T; approve the system extension, then reboot
export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig   # macFUSE ships fuse.pc here
cargo build --release -p bucketbridge-cli --features mount
```

Without the macFUSE development files the build stops in `fuser`'s build script. To only
type-check the code on such a machine use `--features fuser/macos-no-mount` (compiles,
cannot mount).

Windows (MSVC toolchain, Windows SDK):

```powershell
# Install the Dokan 2 runtime (MSI from github.com/dokan-dev/dokany/releases), then:
cargo build --release -p bucketbridge-cli --features mount
pnpm tauri build --features mount
```

`dokan-sys` compiles Dokan's C library from its bundled sources during the build. To link
the installed import library instead, set `DokanLibrary2_LibraryPath_x64` to the `lib`
folder of the Dokan installation. The adapter cannot be cross-checked from macOS or Linux
(the build script runs a Windows executable).

## Use

```bash
mkdir /mnt/photos
bb mount my-bucket /mnt/photos [--prefix 2026/]
# Ctrl-C unmounts; or: fusermount -u /mnt/photos (Linux), umount /mnt/photos (macOS)
```

```powershell
bb mount my-bucket X: [--prefix 2026/]     # a drive letter, or an empty NTFS folder
# Ctrl-C unmounts; or eject the drive in Explorer
```

In the desktop app, right-click a bucket and choose "Mount as drive", pick an empty
folder (on Windows a drive letter from the list or a folder) and press Mount. The entry
appears only when the build supports it and the command `mount_status`
(docs/desktop-api.md) reports it.

## Not supported

Writes, mounting from the Tauri app on a machine without the driver (the dialog
explains what to install), exposing versions or tags as files, and ProjFS or WinFsp
backends.
