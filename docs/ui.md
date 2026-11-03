# Desktop UI (`ui/`)

The React + TypeScript + Vite frontend of the BucketBridge desktop app. It talks
to the Rust host through the IPC contract in [desktop-api.md](desktop-api.md)
and runs unchanged in a plain browser against an in-memory mock backend.

## Stack

| Part | Choice |
|---|---|
| UI | React 19, Mantine 9 (core, hooks, notifications), Tabler icons |
| Lists | `@tanstack/react-virtual` for objects, transfers, log and sync rows |
| State | zustand vanilla stores driven by plain TypeScript controllers |
| Desktop | `@tauri-apps/api` 2.12, plugins dialog 2.8, opener 2.7, clipboard-manager 2.5 (minor versions pinned to the Rust crates) |
| Tooling | Vite 8, TypeScript 6.0 (the newest release typescript-eslint supports), ESLint 10 with typed rules, Vitest 5 + Testing Library, Playwright (Chromium only) |

All dependencies are MIT, Apache-2.0, BSD or ISC licensed.

## Running

```bash
pnpm install                 # from the repository root (pnpm workspace)
pnpm --dir ui dev            # Vite on http://localhost:5173 (strictPort)
```

Outside Tauri the app uses the mock backend, so the browser shows a working
client with demo data. URL parameters change the starting state:

| Parameter | Effect |
|---|---|
| `?mock=empty` | no accounts, no objects (first-run experience) |
| `?mock=locked` | a master password (`correct horse`) is set and locked |
| `?crash=1` | a crash report from the "previous run" is shown |
| `?latency=0` | no artificial command delay (default 120 ms) |
| `?pageSize=50` | listing page size (1..1000) |
| `?manyFiles=10000` | files in the local "Many files" folder (default 10,000) |

Mock credentials (add them with the account wizard):

| Access key ID | Secret | Sees |
|---|---|---|
| `MOCKDEMOACCESSKEY001` | `mock-demo-secret-0001` | the demo buckets |
| `MOCKNEWACCESSKEY0004` | `mock-new-secret-0004` | an empty account |
| `MOCKEXTACCESSKEY0002` | `mock-ext-secret-0002` | endpoint `https://storage.example.com` |
| `MOCKMFAACCESSKEY0003` | `mock-mfa-secret-0003` | MFA account; the only accepted code is `123456` |

Bucket properties: the demo buckets have some configuration (`demo-photos` a
public access block, `demo-backups` versioning and logging, `demo-website` a
website and a policy, `secure-vault` MFA delete). The custom endpoint account
implements policy and versioning only (the other tabs say the server does not
support them) and never transfer acceleration. `partner-exchange` belongs to
another account and is shared with the demo user: it is not listed, so try
"Add external bucket" with its name. Tests simulate refusals with
`mock.failBucketSection(accountId, bucket, section, error)` and a refused
`ListBuckets` with `mock.denyListing(accountId)`.

Local files whose name contains `unreadable` fail with a permission error;
names containing `flaky` fail on the first attempt and succeed on retry or
resume. Pickers, "show in folder" and log export use a simulated file system
shown in an in-app chooser.

### Inside Tauri

The host crate lives in `crates/desktop`; its `tauri.conf.json` uses
`devUrl: http://localhost:5173` and `frontendDist: ../../ui/dist`.

```bash
pnpm tauri dev               # Tauri CLI from the repository root
pnpm --dir ui build          # typecheck, then build ui/dist for `tauri build`
```

In development the host loads the UI from the Vite dev server on port 5173; in
release builds it bundles `ui/dist`.

## Backend switch

`src/main.tsx` checks `isTauri()`:

- inside Tauri it loads `api/tauriBackend.ts` (every command through
  `invoke(name, snake_case_args)`, events through `listen`) and
  `api/tauriPlatform.ts` (dialog, opener and clipboard plugins);
- otherwise it dynamically imports `src/mock`, which is a separate chunk that
  the desktop build never loads. Production code never imports the mock.

## Structure

```
ui/src
  api/          contract types, Backend interface, Tauri backend, BbError
                normalisation, session guard, platform abstraction
  state/        AppModel and its controllers (session, settings, browser,
                transfers, log, sync, dialogs, prompts, status) on zustand stores
  lib/          pure helpers: formatting, S3 keys, selection, sorting,
                passwords, validation, settings ranges, account forms
  components/   shared UI: AppModal (stacking), OK/Cancel and confirm dialogs
                (typed confirmation), wizard, copy field
  features/     screens: buckets, bucketcfg (bucket properties, external
                buckets), objects, objectmeta (the properties editor),
                transfers, sync, accounts, settings, log, about, crash,
                versions (version list and preview), tools (diagnostics,
                unfinished uploads, saved filters, export), menus (context
                menus)
  i18n/         message bundle of the menu text (typed keys)
  app/          App root, theme, main window, toolbar, status bar, styles
  mock/         in-memory backend, simulated file system, browser platform
  test/         test setup and helpers
ui/e2e          Playwright smoke tests
```

### API layer

- `api/types.ts` mirrors every type of the contract.
- `api/backend.ts` declares `Backend`: one function per command, named exactly
  like the command and taking its snake_case argument object (optional values
  are `null`, never omitted), plus `listen(event, handler)`.
- Every rejection is normalised to `BackendError` (`{ code, message }`).
- `api/guard.ts` wraps the backend: a `locked` error opens the unlock dialog
  and retries the call; `mfa_required` opens the MFA prompt (`mfa_login`) for
  the call's `account_id` and retries. Concurrent calls share one dialog.
  Other errors go to the status bar and, for user actions, a notification.

### State and live updates

Settings changes are saved one after another, each on top of the last saved
state; the toolbar's panel and flat-browsing toggles stay disabled until the
settings have been loaded, so a failed load can never write defaults back.

`state/model.ts` creates the controllers, subscribes to the six events and
loads the initial state. Transfer updates, live log entries and sync rows are
buffered and applied once per animation frame, so queueing 10,000 tasks costs
one store update per frame and the virtualised lists render only visible rows.

### Desktop shell rules the UI follows

- Only `invoke` and event `listen`/`unlisten` are used; the UI never emits
  events and does not use the path, window or app modules (`app_info` gives
  version and folders).
- No `eval` or `new Function`, no remote fonts, scripts or images.
- The opener is only used for `https://github.com/anupshaw50/BucketBridge`
  links and for revealing files. Presigned links are copied, not opened.
- The clipboard is write-only. Presigned links and generated passwords use
  `writeSecret`, other values `writeText`. Copy and paste of S3 items uses an
  internal clipboard.
- Every form submit calls `preventDefault()`.

## Properties editor (`features/objectmeta`)

The Properties button opens the editor for the selection. One object: it reads
`get_object_details` and shows tabs General, HTTP headers, User metadata, Tags,
Storage class, Encryption and Permissions, each with its own Apply button and
the result of the last change. Several objects (folders in the selection are left
out): the tabs edit a patch, an empty field keeps each object's value, and every
change shows a table with one result per object (changed, unchanged, or the
error of that object). A single folder shows its name and URI only.

- Tags: one object edits the whole set; several add or overwrite tags, remove
  tags by key, or replace every tag.
- Storage class and encryption copy each object onto itself (the dialog says
  what is kept); an archived object that is not restored shows why it cannot be
  changed.
- SSE-C: an object encrypted with a customer key cannot be read without it. The
  dialog asks for the key, refuses a wrong one with the message of S3, and keeps
  the typed key in memory for the edits that need it (it is never stored or
  written anywhere). The Encryption tab generates a key (`crypto.getRandomValues`)
  and copies it as a sensitive value.
- The rules (tag characters and counts, header values, the 2 KB user metadata
  limit, key formats) live in `lib/objectMeta.ts`, shared with the mock backend,
  which also simulates SSE-C, archived objects and a bucket with ACLs disabled
  (`demo-website`). Demo objects with tags and metadata: `demo-photos/readme.md`,
  `documents/report-q3.pdf`, `demo-website/index.html`.

## Keyboard

| Keys (object list focused) | Action |
|---|---|
| Arrows, Home, End, Page Up/Down (Shift extends) | move the selection |
| Ctrl/Cmd+Click, Shift+Click | toggle, select range |
| Ctrl/Cmd+A, Ctrl/Cmd+I | select all, invert selection |
| Enter, double-click | open folder or object |
| Backspace, Alt+Up | parent folder |
| Delete | delete selection |
| F2 | rename |
| Ctrl/Cmd+C, X, V | copy, cut, paste |
| F3 | preview the selected object |
| F5 (anywhere) | refresh |
| Menu key, Shift+F10 | open the context menu of the focused row |

In the preview of long text: Alt+Left and Alt+Right (previous and next page), Alt+Home and
Alt+End (first and last page).

The list keeps keyboard focus while folders load, so Enter and Backspace can
be chained. Tab reaches the column headers, where Enter or Space sorts. The
bucket tree supports arrows (Left/Right collapse and expand), Home, End, Enter
and Space. In the tree, F5 reloads the list of buckets, Delete deletes the
bucket (after typing its name), and Ctrl click (Cmd click on macOS) marks
several buckets for one menu.

## Context menus

Right-click opens a menu on a bucket, a folder, a file, several selected rows,
the empty area of the object list, the empty area of the bucket pane, and an
account (rows of the account manager and the account button of the toolbar).
Ctrl+click opens it on macOS as well, but not when the pointer was dragged
between press and release; elsewhere Ctrl+click keeps adding to the selection.
The menu is built from data that is already loaded and never calls the backend
while it is open.

`src/features/menus`:

| File | Role |
|---|---|
| `ids.ts` | every action id (in use, and planned), menu ids, hint ids |
| `registry.ts` | the action registry: one handler per id, visibility (`visible`, `many`, `requires`), enabled state (`disabled`), and `runAction`; the handlers call the same functions as the toolbar (`objects/actions.ts`) |
| `layouts.ts` | the menus as data: which ids, in which order, with which separators and submenus |
| `resolve.ts` | turns a layout and a context into the entries to show: hidden, disabled or enabled, with label and shortcut |
| `context.ts` | the context of a menu (targets, place, capabilities from the account and bucket) |
| `keymap.ts` | the only list of shortcuts; menus show them from here and the pane key handlers resolve key events through it, so a shown shortcut always works |
| `available.ts` | the feature availability map, see "Which entries are live" below |
| `ContextMenu.tsx`, `useContextMenu.ts` | the portal menu: clamping, focus return, arrows, Home, End, type-ahead, submenus with a 200 ms hover delay, closing on Escape, outside press, blur, scroll and resize |
| `surfaces.ts` | `bucketMenu`, `itemsMenu`, `filesEmptyMenu`, `bucketsEmptyMenu`, `accountMenu` for the components |

Rules the menus follow:

- One registry: the context menus, the toolbar twins (copy, paste, delete,
  open, rename, properties, share, export, preview, versions, new folder, upload,
  selection, refresh, sync, the bucket menu, the Tools menu, "Manage filters")
  and the keyboard handlers of the panes run the same action with the same
  enabled rules. A toolbar button that has a menu twin takes its enabled state,
  reason and shortcut text from the registry and the keymap (F3 is Preview).
  Controls that pick a value (the saved filter list, Download to a default
  folder) stay toolbar parts that call registry actions for what they open.
- Entries that the endpoint cannot do are hidden (`requires`, from
  `BucketCaps`: an account with a custom endpoint is an external provider);
  entries that the current state does not allow (paste with an empty
  clipboard, select all in an empty folder) are shown disabled with the reason
  as their tooltip.
- Several selected rows or buckets show only the entries marked `many`.
- Deleting asks first: the typed bucket name, the typed account label, or the
  existing delete confirmation with the count and the size.
- Work goes through the existing enqueue commands, so it appears in the
  transfer queue.
- Menu text lives in `src/i18n/en.ts`, keyed by action id. A label that is
  missing or misspelled does not compile.

### Which entries are live

`available.ts` holds a flag for every entry that was planned before its feature
existed. A `false` flag hides the entry in every menu, the toolbar and the
keymap; a `true` flag needs a definition in `PLANNED_DEFS` in `registry.ts`
(a test fails when a flag is on and the action has no handler). The handlers
reuse the action functions of the features, never a copy of their logic.

| Feature | Entries now live | Opens |
|---|---|---|
| Bucket configuration (T03) | Properties submenu of a bucket: Versioning, Server access logging, Bucket policy, Public access block, Static website, Website routing rules, Transfer acceleration. Add external bucket (empty area of the buckets pane, Bucket actions button). Remove external bucket (bucket menu, only for an external bucket of the list) | Bucket properties on the tab of the setting (routing rules are on the Website tab); the external bucket dialog |
| Object metadata (T06) | Properties (also for several selected rows, from the toolbar), Metadata submenu: Tags, Storage class, Server-side encryption, HTTP headers; ACL | The properties editor on the General, Tags, Storage class, Encryption, HTTP headers or Permissions tab. With several objects selected, Metadata offers Storage class and Tags and edits all of them together |
| Versions and preview (T08) | Preview (F3), Versions (file, folder and empty-area menus) | The preview, the version list of the object, the folder or the open folder |
| Windows and bookmarks (T14) | Open in new window (bucket and folder menus), Bookmark (bucket, folder and file menus) | Another main window at that location (`open_new_window`); the bookmark dialog. Saved bookmarks list in the Bookmarks section above the bucket tree: click opens, the pencil renames, the bin deletes after a confirmation |
| Edit in place (v0.2.0) | Edit (file menu, one object) | Downloads the object, opens the copy and uploads every save; the status bar shows Editing N, which opens the Editing panel; a conflict opens the conflict dialog. See "Edit in place" |
| Tools and export (T12) | Export (file and selection menus, and the Export listing button of the toolbar), Filter (folder and empty-area menus), Diagnostics and Uncompleted multipart uploads (empty area of the buckets pane), Test connection (account menu) | The export dialog for the open folder, the saved filters dialog (the folder of the menu is opened first), diagnostics (the account that was clicked is preselected), the unfinished uploads |

Entries that depend on the endpoint are hidden through `BucketCaps`: on an
account with a custom endpoint, Versioning, Static website, Website routing
rules and Transfer acceleration do not show; logging, policy and public access
block do, and the dialog reports what the provider refuses. Export always
exports the listing of the folder that holds the selection, with the saved
filter in effect, and not only the selected rows.

CloudFront bucket entries: Attach distribution, Edit distribution and Detach
distribution open the CloudFront manager (`CloudFrontDialog`) with the matching
form. Attach asks for the bucket region, comment, default root object, price
class and enabled flag, then shows the bucket policy to add. Edit changes the
comment, aliases, default root object, price class and enabled flag. Detach
removes only the bucket's origin from the distribution, after typing the bucket
name.

Still hidden, with their flags `false`: CORS, Lifecycle rules,
Intelligent-Tiering, Replication, Object ownership and Object lock (M3),
Encrypt, Decrypt,
Lock and Restore from Glacier (M3), CloudFront and its submenu, IAM manager,
CloudFront manager, Mount as drive (no platform supports it yet),
CloudFront manager, Bookmark, Open in new window,
Open with, Move to, Copy to, Copy all files, Move all files, Sharing wizard,
Paste into a bucket and Open in browser. To wire one of them: add its
definition to `PLANNED_DEFS`, then turn its flag on.

"Mount as drive" is wired (`features/mount/`) but its entry shows only when
`mount_status().supported` is true, which needs a build with the `mount` feature
(docs/mount.md); the mock backend reports `false` unless started with
`mountSupported: true`.
CloudFront manager, Mount as drive (no platform supports it yet), Bookmark, Open in new window,
Move to, Copy to, Copy all files, Move all files, Sharing wizard,
and Paste into a bucket. To wire one of them: add its
definition to `PLANNED_DEFS`, then turn its flag on.

Open with, Website redirect and Open in browser (`features/fileopen/`): Open with
(one file) lists the recent applications (`settings.general.open_with_recent`)
and a "Choose application..." button for the native picker
(`Platform.pickApplication`), then calls `open_object_with`. Website redirect
(one file) reads the location from the object details and saves it with
`update_object_headers`; an empty field removes it, and anything but a `/path`
or `http(s)://` URL is refused before saving. Open in browser (one bucket)
calls `open_bucket_in_browser`, so the opener capability needs no URL scope.

## Edit in place (`features/edit`)

- **Edit** in the file menu (one object; several selected objects do not offer
  it) calls `model.edits.start`, which runs `start_edit` with the default
  application and shows "Editing <key>" in the status bar. There is no
  "Edit with" entry; "Open with" stays a one-off copy.
- `state/edits.ts` (`EditController`) keeps the active sessions in step through
  `edit://changed` (a `stopped` payload removes a session) and loads
  `list_edits` at startup. A session that turns `conflict` opens the conflict
  dialog once.
- **Status bar.** While a session exists, "Editing N" shows next to the
  transfer counts, in red with "M need attention" when a session is in
  conflict or has failed. Clicking it opens the **Editing** panel: a table of
  the sessions with their state (Waiting for a save, Uploading, Uploaded,
  Conflict, Upload failed), Resolve... for a conflict, Retry for a failed
  upload and Stop for each. Stop uploads a pending save; with a conflict it
  fails and the status bar says to resolve it first.
- **Conflict dialog.** Names the object and says whether it changed or was
  deleted. Three choices: Save as a copy (shows the key it will use, nothing
  to confirm), Overwrite and Discard my changes (each asks for confirmation,
  Overwrite warns that the change on the server is lost). Decide later closes
  the dialog and keeps the session; further saves stay held. The dialog closes
  itself when the session ends or the conflict is settled elsewhere.
- **Mock backend** (`mock/editMock.ts`): sessions over the mock world with the
  same rules (ETag compare, deleted object, `name (edited).ext` that does not
  overwrite). There is no file watcher; `mock.saveEdit(id)` plays the editor
  saving the copy, `mock.changeObject` and `mock.deleteObject` play somebody
  else changing the object.

## Versions and preview

`features/versions/`: the toolbar's Versions button opens the versions of the selected object
(exactly that key), of the selected folder, or of the open folder when nothing single is
selected. The dialog lists keys with their versions and delete markers (marked and tinted, with
a Deleted badge on keys whose newest entry is a marker), loads more pages on request, and
applies the filters only with Apply (show versions on or off, delete markers included, hidden or
only, keys with several versions, more or fewer than N versions, a date range of whole days; the
same rules as the backend, checked first in `filters.ts`). Each version has Preview, Download
(a save dialog, then a download task of the transfer queue), Restore (not for the current
version) and Purge; a delete marker can only be removed. Purging, one version or the checked
ones, asks to type `purge`. The toolbar's Preview button and F3 open the preview of the selected
object: text and JSON as text in a `<pre>` (never markup), images from a `blob:` URL (`data:` when
blob URLs are unavailable; only `image/*` types), the states too large, binary and empty as
messages, and for long text First, Previous, Next and Last, a page number and a page size that
takes effect with Apply.

The mock backend (`mock/versions.ts`) keeps a version history per key (what happened to the
live objects since, such as an overwrite or a delete, is added when a key is read) and
generates preview content: `documents/notes.txt`, `documents/old-draft.txt` (deleted),
`readme.md` and `index.html` have histories; `demo-logs` holds `config.json`,
`audit-export.csv` (600 KiB, 5 pages), `monitoring-dashboard.webp`, `site-photo-large.png`
(too large), `snapshot.bin` and `empty.txt`; images of any other type are drawn as a PNG.

## Settings validation

The dialog checks the same ranges as core's `Settings::validate` (documented in
the contract) before calling `save_settings`, and marks the pages with problems:

| Setting | Range |
|---|---|
| advanced.max_attempts | 1 to 20 |
| advanced.connect_timeout_secs | 1 to 300 |
| advanced.read_timeout_secs | 1 to 3600 |
| advanced.list_page_size | 1 to 1000 |
| logging.max_file_mib | 1 to 1024 |
| proxy.port | 1 to 65535 |
| transfer.max_concurrent_tasks, max_parts_per_task | 1 to 32 |
| transfer.multipart_threshold_mib, part_size_mib | 5 to 5120 |
| transfer.bandwidth_limit_kibps | null (unlimited) or 1 to 104,857,600 |
| transfer.retry_attempts | 0 to 20 |

The proxy host is required when the proxy is enabled and cannot contain
whitespace; a user name, when set, is non-empty; bypass entries must be host
names, `*.suffix` patterns, IP addresses or CIDR ranges.

## Contract rules the UI and the mock follow

- Folder semantics: selected names land directly in the destination, so the
  UI refuses a paste or download when two selected items would get the same
  name there (for example two `budget 2026.xlsx` files picked in flat view),
  and the backend rejects such enqueue commands with `conflict` without
  queueing anything.
- Queue: pause applies to queued or running tasks, resume to stopped ones,
  retry to failed ones, and cancel to queued, running, stopped or failed ones;
  the per-task buttons are enabled only in those states. Any other transition
  is rejected with `conflict`. `clear_finished` removes completed and cancelled
  tasks; failed tasks stay until retried or cancelled.
- Sync: rows whose analysis failed have the action `failed`, show why, and are
  never executed. A row that fails while executing keeps its planned action,
  stays not done and shows its error; Resume (`sync_execute` again) re-runs
  every copy or delete row that is not done and clears the error on success.
  Progress covers the whole plan: total = copy and delete rows, done = those
  applied, failed = failed rows plus not-done rows with an error. "Failed only"
  shows rows whose action is failed or whose error is set. Deleting is
  disabled for bidirectional sync (the backend rejects it with `usage`).
- Sync jobs (v0.2.0): the Folder sync dialog asks for the direction (now also "S3 to S3
  (between prefixes)", which asks for a source bucket and folder and a destination bucket
  and folder instead of a local folder), the folders, and, under "Filters and options",
  exclude rules (gitignore style, one per line), include globs, an optional exclusion
  file, "Skip empty folders", "Skip duplicate content", "Use the local metadata cache" and
  the parallel listing. "Save as job" (on the folders step and under the results) names the
  setup and stores it. The "Sync jobs" entry of the bucket and folder menus lists the saved
  jobs: Run analyses the job and opens the plan for review in the sync dialog (nothing is
  copied until Execute; a job of another account is run after switching to that account),
  Edit opens the form filled from the job and "Save job" replaces it, and Delete asks for
  confirmation and removes only the saved setup. When a run ends the results show a
  summary: counts and bytes per action, the duration and the failures.
- Log: `get_log` matches the text case-insensitively in message and source,
  `levels: null` means all levels, and `limit` keeps the newest entries; the
  panel asks for the newest 10,000 (all the backend keeps) and appends live
  entries that match the same filter.
- Listings: each page lists prefixes first, then objects; etags come without
  quotes and listings carry no checksums. In flat listings nested folder
  markers are ordinary entries with `is_prefix: false`; the UI treats any key
  ending in `/` as a folder.

## Testing

```bash
pnpm --dir ui typecheck
pnpm --dir ui lint           # zero warnings allowed
pnpm --dir ui test           # Vitest: unit + component tests (jsdom)
pnpm --dir ui exec playwright install --only-shell chromium   # once
pnpm --dir ui e2e            # Playwright smoke tests on port 5183
```

Component tests render the whole app against the mock backend with its clock
stopped; tests advance simulated time with `advance(ms)`. The e2e tests start
their own Vite server on port 5183, so they never attach to a dev server on
5173.

## Tools and export

The T12 tools (diagnostics, uncompleted multipart uploads, saved filters and the
export of a listing) live in `ui/src/features/tools/`; the commands they call are
in "Tools and export (T12)" of [desktop-api.md](desktop-api.md).

| File | What it is |
|---|---|
| `DiagnosticsPanel.tsx` | Runs `run_diagnostics` for a stored account or an unsaved `AccountInput` and shows the verdict, one row per step (status badge, timing, summary, detail, outcome code) and a "Copy report" button. The unsaved input goes straight into the command; no store keeps it. |
| `DiagnosticsDialog.tsx` | "Diagnostics": account selector (the active account by default), optional bucket for the region probe (the bucket list as suggestions), and the panel. Changing the account or the bucket drops the report, which would be out of date. |
| `UploadsDialog.tsx` | "Uncompleted multipart uploads": buckets of the active account (the open bucket is listed at once, "All buckets" picks every one), a table with checkboxes, "select older than N days", abort after a confirmation, a per-upload results table, and an alert per bucket that could not be listed. |
| `FiltersDialog.tsx`, `filterDraft.ts` | "Saved filters": list, create, edit (renames included), delete after a confirmation and "Apply". The form validates with the TypeScript engine as you type (a glob error shows at its field) and previews how many of the loaded items the filter keeps; the backend has the last word, and its error (a taken name, for example) shows on the form. |
| `FilterControls.tsx` | The toolbar's filter menu (saved filters with a check on the applied one, "Clear saved filter", "Manage filters...") and the chip next to the item count. |
| `ExportDialog.tsx` | "Export listing": format (CSV, Excel workbook, Parquet), "Include everything below this folder" (on by default in flat browsing), "Only entries matching the active filter" while a filter is applied, then the native save dialog (`pickSavePath`, a default name such as `demo-photos-photos_2024.csv`, the format's extension filter). A cancelled dialog exports nothing; success shows the row count and size with "Show in folder". |
| `ToolsMenu.tsx`, `actions.ts` | The toolbar's "Tools" menu and the action that opens the export dialog for the open folder. |

Entry points: the toolbar has an "Export listing" button (enabled while a bucket is
open), a filter menu (button "Apply a saved filter") and a "Tools" menu
(Diagnostics..., Uncompleted uploads..., Saved filters...). The add-account
wizard's Review step has "Test connection", which probes the account that is
about to be added without storing it. The dialogs are the dialog kinds
`diagnostics`, `uploads`, `filters` and `export` of `state/dialogs.ts` (singletons).

State: `model.filters` (`state/filters.ts`) holds the stored filters, reloaded at
start and whenever the filter menu or the dialog opens, and keeps an applied
filter in step when it is edited, renamed or deleted. The filter that is applied
is `BrowserState.filter` (`BrowserController.setFilter`): `recompute()` narrows
the loaded entries with it before the text search and the sort, it stays while
folders are browsed (paths are relative to the open folder) and goes when the
account changes. With a filter or a search the item count reads "N of M loaded".

### The TypeScript filter engine

`lib/filters.ts` is a port of `crates/core/src/filter.rs`, because the object list
filters over entries that are already loaded and must give the answers of the
engine that validates the saved filters. Paths are relative to the listing prefix;
`prefix` is a plain string prefix; size bounds are inclusive; `modified_after` is
inclusive and `modified_before` exclusive; storage classes compare ignoring case
and a missing class is `STANDARD`; folder entries match when something below them
can satisfy the prefix. Globs are parsed token by token like the `globset` parser
the engine uses (`*`, `**` as a whole segment, `?`, `[a-z]`, `[!x]`, `{a,b}` with
nesting, `\` escapes, empty alternates dropped), compiled to a regular expression
and matched on the UTF-8 bytes of the path (one character per byte), as the Rust
regex matches bytes: `?` is one byte, a class never matches `/`, matching is
case-sensitive and a backslash in a key is never a separator. Error messages use
the engine's wording. `lib/filters.test.ts` repeats the vectors of the Rust tests
of `filter.rs` (stars, classes, alternates, escapes, case, bytes, prefix pruning,
size, date and class rules, folder handling) plus the parser's edge cases; when the
Rust engine changes, change the port and these tests with it. The port is never
the authority for what may be stored: `save_filter` and `export_listing` compile
the spec with the Rust engine.

### Mock behaviour of the tools commands

`mock/toolsMock.ts` holds the state; `MockBackend` delegates after its latency.

- `run_diagnostics`: exactly one of `account_id` and `input` (else `usage`); a
  stored account rejects with `locked` or `mfa_required` like any account command,
  an invalid region, endpoint or empty key rejects with `usage`. The report has the
  six steps in order with stable pseudo-random timings. Demo outcomes: the
  "Wrong secret" account gives `credentials` / `bad_signature`, an unknown access
  key `invalid_access_key`, a bucket of another owner `region` / `access_denied`,
  an unknown bucket `bucket_not_found`; a custom endpoint other than
  `https://storage.example.com` does not resolve (`dns` / `not_found`, later steps
  `blocked`), an IP address endpoint without a server is refused (`tcp` /
  `refused`), a plain-http endpoint skips `tls`. With the proxy enabled in the
  settings, `dns` and `tcp` describe the proxy host and `proxy` passes (a bypassed
  host skips it with `direct`). Reports never contain a secret.
- Uploads: a fixture of in-progress uploads in `demo-backups`, `demo-photos`,
  `demo-logs`, `demo-website` and the custom-endpoint bucket `archive-2025`, aged
  2 to 200 days relative to when the mock started (so "older than 30 days" is
  stable whenever the tests run). Listing `demo-website` fails with
  `AccessDenied` (a bucket policy), which exercises the per-bucket failures; an
  upload that is already gone aborts with `not_found` in its result.
- Saved filters: the rules of the contract (trimmed names of 1 to 100 characters,
  unique ignoring case, `replaces` for edits and renames, at most 200, specs
  validated with the TypeScript engine so a bad glob is a `usage` error).
- `export_listing`: `usage` for a relative path, `io` for a directory or a missing
  folder, otherwise it writes a file into the simulated file system whose size is
  that of the CSV text (including the formula-defusing apostrophe) or an estimate
  for the other formats, counting the rows of the mock listing like the backend
  (folders first for one level, every object below the prefix when recursive, the
  filter applied relative to the prefix) and logging an entry.

Tests: `lib/filters.test.ts`, `lib/diagnostics.test.ts` (the copyable report),
`lib/tools.test.ts`, `mock/toolsMock.test.ts` and the component tests in
`features/tools/tools.test.tsx` (every dialog, the toolbar entry points, the
wizard's "Test connection" and applying and clearing a filter on the object list).

## Bookmarks and new windows (`features/bookmarks`)

`BookmarkController` (`state/bookmarks.ts`) keeps the bookmark list in a store.
It loads at startup and follows the `bookmarks://changed` event, so a bookmark
added in one window shows in every window. Opening a bookmark makes its account
the active one and opens its folder (a bookmarked object opens the folder that
holds it). `BookmarksSection` renders the list above the bucket tree and hides
itself while the list is empty. Bookmark and Open in new window are registry
actions (`bookmark.add`, `window.open-new`) with no keyboard shortcut.

A window opened with `open_new_window` reads `bb_account`, `bb_bucket` and
`bb_prefix` from its URL (`lib/startLocation.ts`) and passes them to the
`AppModel` as `startAt`; after the accounts have loaded the model opens that
location, or ignores it when the account no longer exists. The mock backend
records the requested windows in `openedWindows`.
## Sharing wizard and URL shorteners

`features/sharing/` holds the bucket sharing wizard (`SharingWizard`: object, access,
link; opened from the bucket menu entry `bucket.sharing-wizard`), the `ShortenControl`
used by the wizard and by the "Share link" dialog, and `ShortenerSettings` (the Sharing
page of the settings). The Shorten button only appears when a service is chosen in the
settings; its caption says the whole link leaves the machine. Tests:
`features/sharing/sharing.test.tsx`. Contract: "Sharing wizard and URL shorteners" in
`docs/desktop-api.md`.

## Overwrite policy, modification times and accounts in the picker (`features/transfers`, `features/picker`)

* Settings: "When the destination exists" (page Transfers, `transfer.if_exists`)
  and "Keep modification times" (page Data integrity,
  `data_integrity.preserve_mtime`).
* The destination picker has a "Destination account" select (shown when there
  are several accounts) and "If an item already exists there" (default: as set
  in Settings). Choosing another account lists its buckets; the dialog notes
  that the service copies when it can and that otherwise the data goes through
  this computer, and that a move deletes each source only after its copy was
  checked. `destinationProblem(..., sameAccount)` allows a bucket of the same
  name in another account and only refuses name clashes there.
* `ConflictHost` (`ConflictDialog.tsx`, mounted next to `DialogHost`) shows the
  first overwrite question of a task that waits (`transfer://conflict`,
  `list_conflicts`, held in `model.conflicts`): the source and the existing
  item with size and time, buttons Overwrite, Overwrite if newer, Rename and
  Skip, "Apply to all remaining in this batch" and "Cancel this transfer". The
  transfer panel shows such a task as "Waiting".
* A task that was skipped has the state `skipped` and the badge "Skipped"; its
  reason is shown as "Skipped: ...". "Clear finished" removes it.
* The mock implements all of it (`MockBackend.placeTask`): policies, renames,
  "newer" by `last_modified`, questions and batch answers, and copies between
  accounts.

## Credential sources (`features/accounts`)

The add-account wizard and the edit dialog handle every kind of source; the
Accounts manager has an "Import from AWS config..." button, and the account menu
of the toolbar the same entry.

- **Add account.** Step 1 (Connection) is unchanged. Step 2 (Credentials) starts
  with a "Credentials from" control: *Access keys* (the fields of version 0.1.0),
  *Assume a role*, *AWS SSO* and *Instance role*. The review step lists the
  settings of the chosen kind and says that no secret is stored; "Test
  connection" is offered for access keys only.
  - *Assume a role* (`RoleFields`): the account that signs the request, the role
    ARN, the session name, the session length in minutes (15 to 720), an optional
    external ID and an optional MFA device. Each field is validated before the
    next step (`validateCredentials` in `lib/accounts.ts`, the same rules as the
    backend).
  - *AWS SSO* (`SsoFields`): start URL, SSO region and an optional session name,
    then "Sign in with AWS SSO". The sign-in shows the user code and the
    verification link (the page is opened in the browser through the host; the
    link stays visible when that failed), polls `sso_poll_login` at the interval
    the backend asks for, and "Cancel sign-in" stops it. When it completes the
    "AWS account" and "Role" lists are filled from `sso_list_accounts` and
    `sso_list_roles`; a single role is chosen automatically. Closing the dialog
    cancels a sign-in that is waiting. The token goes to the AWS CLI cache, never
    through the UI state.
  - *Instance role* (`InstanceFields`): "Check this machine" calls
    `probe_instance_role`; the account cannot be added before the check succeeds,
    and a failure (not on EC2 or in a container) is shown with the reason.
- **Edit account.** An assumed role can change its session name, length,
  external ID and MFA device (sent as `source`); the source account, an SSO role
  and a linked AWS profile are shown read-only (add a new account to use another
  role). Access keys keep the secret-rotation fields. The MFA prompt that follows
  a change to a role's MFA device is the normal `mfa_required` flow.
- **Account list.** The "Credentials" column shows the key ID for access keys and
  otherwise `AWS profile NAME`, `Role NAME through ACCOUNT`, `SSO ROLE in ACCOUNT`
  or `Instance or container role` (`credentialsLabel`).
- **Import from AWS config** (`ImportAwsDialog`): lists the profiles with their
  type and region; usable profiles start selected, profiles that are linked
  already show "Already imported", and unusable ones are disabled with the
  reason. "Import" links the chosen ones and reports what was left out.
- **Mock** (`mock/credentialsMock.ts`): four profiles (`dev`, `prod-admin`,
  `corp-sso`, `broken`), an instance role that tests can switch off
  (`mock.credentials.instanceRoleAvailable`), and a portal with two AWS accounts
  whose sign-in completes after `pendingPolls` pending answers. Accounts of every
  source browse the demo identity; a role with an MFA device asks for the demo
  code.
