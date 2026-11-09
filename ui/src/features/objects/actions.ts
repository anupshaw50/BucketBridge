import type { IfExists, ObjectEntry, TransferTask } from "../../api/types";
import { tr } from "../../i18n";
import { formatBytes, formatCount } from "../../lib/format";
import { isFolderEntry } from "../../lib/objects";
import { downloadProblem, pasteProblem } from "../../lib/s3keys";
import { invert, orderedSelection, selectAll } from "../../lib/selection";
import type { ObjectPropertiesTab } from "../../state/dialogs";
import type { AppModel, ClipboardContents } from "../../state/model";

export interface OpenLocation {
  accountId: string;
  bucket: string;
  prefix: string;
}

/** A folder of a bucket (the bucket root has an empty prefix). */
export interface Place {
  bucket: string;
  prefix: string;
}

/** The items an action works on: rows of one bucket, either selected in the list or named by a menu. */
export interface Subject {
  accountId: string;
  bucket: string;
  entries: readonly ObjectEntry[];
}

/** Id of the filter box above the object list, so that "Search" can focus it. */
export const SEARCH_INPUT_ID = "bb-search-input";

/** The bucket and folder shown in the object list, if any. */
export function openLocation(model: AppModel): OpenLocation | null {
  const { listing } = model.browser.state;
  if (listing.accountId === null || listing.bucket === null) {
    return null;
  }
  return { accountId: listing.accountId, bucket: listing.bucket, prefix: listing.prefix };
}

/** Where an action lands: the given place of the active account, or the open folder. */
function locationOf(model: AppModel, at?: Place): OpenLocation | null {
  if (at === undefined) {
    return openLocation(model);
  }
  const accountId = model.session.store.getState().activeAccountId;
  return accountId === null ? null : { accountId, bucket: at.bucket, prefix: at.prefix };
}

export function selectedEntries(model: AppModel): ObjectEntry[] {
  return model.browser.selectedEntries();
}

export function selectedKeys(model: AppModel): string[] {
  const { selection, visibleKeys } = model.browser.state;
  return orderedSelection(selection, visibleKeys);
}

/** The selected rows of the open folder, or null when nothing is selected. */
export function selectionSubject(model: AppModel): Subject | null {
  const location = openLocation(model);
  const entries = selectedEntries(model);
  if (location === null || entries.length === 0) {
    return null;
  }
  return { accountId: location.accountId, bucket: location.bucket, entries };
}

async function enqueue(model: AppModel, what: string, run: () => Promise<TransferTask[]>): Promise<boolean> {
  try {
    const tasks = await run();
    model.transfers.addReturned(tasks);
    const label = tr(what);
    model.status.info(
      tasks.length === 1
        ? tr("{what}: 1 task added to the transfer queue.", { what: label })
        : tr("{what}: {n} tasks added to the transfer queue.", { what: label, n: formatCount(tasks.length) }),
    );
    return true;
  } catch (error) {
    model.status.error(error, { notify: tr("{what} could not be started", { what: tr(what) }) });
    return false;
  }
}

async function pick<T>(model: AppModel, run: () => Promise<T | null>): Promise<T | null> {
  try {
    return await run();
  } catch (error) {
    model.status.error(error, { notify: tr("The file dialog could not be opened") });
    return null;
  }
}

export async function uploadFiles(model: AppModel, at?: Place): Promise<void> {
  const location = locationOf(model, at);
  if (location === null) {
    return;
  }
  const paths = await pick(model, () => model.platform.pickFiles(tr("Choose files to upload")));
  if (paths === null || paths.length === 0) {
    return;
  }
  await enqueue(model, "Upload", () =>
    model.api.enqueue_upload({
      account_id: location.accountId,
      local_paths: paths,
      bucket: location.bucket,
      prefix: location.prefix,
    }),
  );
}

export async function uploadFolder(model: AppModel, at?: Place): Promise<void> {
  const location = locationOf(model, at);
  if (location === null) {
    return;
  }
  const folder = await pick(model, () => model.platform.pickFolder(tr("Choose a folder to upload")));
  if (folder === null) {
    return;
  }
  await enqueue(model, "Upload", () =>
    model.api.enqueue_upload({
      account_id: location.accountId,
      local_paths: [folder],
      bucket: location.bucket,
      prefix: location.prefix,
    }),
  );
}

/** Asks for (or reads the default) download folder, then queues the downloads of `keys`. */
async function downloadKeys(model: AppModel, accountId: string, bucket: string, keys: string[], choose: boolean) {
  // The selected names land directly in one folder, so two items with one name would overwrite each other.
  const problem = downloadProblem(keys);
  if (problem !== null) {
    model.status.error(problem, { notify: tr("Download not started") });
    return;
  }
  const fallback = model.settings.current.general.default_download_dir;
  const destination =
    !choose && fallback !== null
      ? fallback
      : await pick(model, () => model.platform.pickFolder(tr("Choose the download folder")));
  if (destination === null) {
    return;
  }
  await enqueue(model, "Download", () =>
    model.api.enqueue_download({ account_id: accountId, bucket, keys, dest_dir: destination }),
  );
}

/** Downloads the subject (default: the selection) to the default folder, or asks when there is none (or when `choose`). */
export async function downloadSelected(
  model: AppModel,
  choose: boolean,
  subject: Subject | null = selectionSubject(model),
): Promise<void> {
  if (subject === null || subject.entries.length === 0) {
    return;
  }
  await downloadKeys(
    model,
    subject.accountId,
    subject.bucket,
    subject.entries.map((entry) => entry.key),
    choose,
  );
}

/** Lists every page of one level of a bucket (folders expand when they are downloaded). */
async function listLevel(model: AppModel, accountId: string, bucket: string, prefix: string): Promise<string[] | null> {
  const keys: string[] = [];
  let token: string | null = null;
  try {
    do {
      const page = await model.api.list_objects({ account_id: accountId, bucket, prefix, flat: false, token });
      keys.push(...page.entries.map((entry) => entry.key));
      token = page.next_token;
    } while (token !== null);
  } catch (error) {
    model.status.error(error, { notify: tr("The bucket could not be listed") });
    return null;
  }
  return keys;
}

/** Queues the download of everything in a bucket, whether or not it is open or fully loaded. */
export async function downloadAll(model: AppModel, bucket: string): Promise<void> {
  const accountId = model.session.store.getState().activeAccountId;
  if (accountId === null) {
    return;
  }
  const keys = await listLevel(model, accountId, bucket, "");
  if (keys === null) {
    return;
  }
  if (keys.length === 0) {
    model.status.info(tr("{bucket} is empty: there is nothing to download.", { bucket }));
    return;
  }
  await downloadKeys(model, accountId, bucket, keys, true);
}

export async function deleteSelected(
  model: AppModel,
  subject: Subject | null = selectionSubject(model),
): Promise<void> {
  if (subject === null || subject.entries.length === 0) {
    return;
  }
  const { accountId, bucket, entries } = subject;
  if (model.settings.current.general.confirm_delete) {
    const folders = entries.filter((entry) => isFolderEntry(entry)).length;
    const bytes = entries.reduce((sum, entry) => sum + (isFolderEntry(entry) ? 0 : entry.size), 0);
    const what =
      entries.length === 1
        ? tr("\"{key}\"", { key: entries[0]?.key ?? "" })
        : `${tr("{n} items", { n: formatCount(entries.length) })}${bytes > 0 ? tr(" ({size})", { size: formatBytes(bytes) }) : ""}`;
    const message = tr("Delete {what} from {bucket}?", { what, bucket });
    const confirmed = await model.dialogs.confirm({
      title: tr("Delete"),
      message:
        folders > 0
          ? `${message}\n${tr("Folders are deleted together with everything inside them. This cannot be undone.")}`
          : `${message}\n${tr("This cannot be undone.")}`,
      confirmLabel: tr("Delete"),
      danger: true,
    });
    if (!confirmed) {
      return;
    }
  }
  await enqueue(model, "Delete", () =>
    model.api.enqueue_delete({ account_id: accountId, bucket, keys: entries.map((entry) => entry.key) }),
  );
}

export function newFolder(model: AppModel, at?: Place): void {
  const location = locationOf(model, at);
  if (location !== null) {
    model.dialogs.open({ kind: "new-folder", ...location });
  }
}

export function renameSelected(model: AppModel, subject: Subject | null = selectionSubject(model)): void {
  const entry = subject?.entries[0];
  if (subject !== null && subject.entries.length === 1 && entry !== undefined) {
    model.dialogs.open({ kind: "rename", accountId: subject.accountId, bucket: subject.bucket, key: entry.key });
  }
}

export function copySelection(
  model: AppModel,
  mode: "copy" | "cut",
  subject: Subject | null = selectionSubject(model),
): void {
  if (subject === null || subject.entries.length === 0) {
    return;
  }
  const keys = subject.entries.map((entry) => entry.key);
  model.clipboard.setState({
    contents: { accountId: subject.accountId, bucket: subject.bucket, keys, mode },
  });
  const count = formatCount(keys.length);
  model.status.info(
    mode === "copy"
      ? keys.length === 1
        ? tr("1 item copied. Open the destination folder and paste.")
        : tr("{n} items copied. Open the destination folder and paste.", { n: count })
      : keys.length === 1
        ? tr("1 item cut. Open the destination folder and paste.")
        : tr("{n} items cut. Open the destination folder and paste.", { n: count }),
  );
}

/** Why pasting `contents` into `location` is not possible, or null when it is. */
export function pasteBlockerFor(contents: ClipboardContents | null, location: OpenLocation | null): string | null {
  if (contents === null) {
    return tr("Copy or cut items first.");
  }
  if (location === null) {
    return tr("Open a bucket to paste into.");
  }
  if (contents.accountId !== location.accountId) {
    return tr("The clipboard holds items from another account.");
  }
  return pasteProblem(contents.bucket, contents.keys, location.bucket, location.prefix);
}

export function pasteBlocker(model: AppModel, at?: Place): string | null {
  return pasteBlockerFor(model.clipboard.getState().contents, locationOf(model, at));
}

/** Queues a copy or move of `keys` into a folder of the same account. Returns whether the tasks were queued. */
export async function transferKeys(
  model: AppModel,
  mode: "copy" | "move",
  args: {
    account_id: string;
    src_bucket: string;
    keys: string[];
    dst_bucket: string;
    dst_prefix: string;
    /** The account that owns the destination bucket; default: `account_id`. */
    dst_account_id?: string | null;
    /** Overrides the overwrite policy of the settings for this transfer. */
    if_exists?: IfExists | null;
  },
): Promise<boolean> {
  return mode === "copy"
    ? enqueue(model, "Copy", () => model.api.enqueue_copy(args))
    : enqueue(model, "Move", () => model.api.enqueue_move(args));
}

/** Pastes the clipboard into a folder (default: the open folder). */
export async function paste(model: AppModel, at?: Place): Promise<void> {
  const contents = model.clipboard.getState().contents;
  const location = locationOf(model, at);
  const blocker = pasteBlocker(model, at);
  if (blocker !== null || contents === null || location === null) {
    if (blocker !== null) {
      model.status.error(blocker, { notify: tr("Paste not started") });
    }
    return;
  }
  const ok = await transferKeys(model, contents.mode === "copy" ? "copy" : "move", {
    account_id: location.accountId,
    src_bucket: contents.bucket,
    keys: contents.keys,
    dst_bucket: location.bucket,
    dst_prefix: location.prefix,
  });
  if (ok && contents.mode === "cut") {
    model.clipboard.setState({ contents: null });
  }
}

/** Opens a folder in the list, or a file with the default application. `bucket` defaults to the open one. */
export async function openEntry(model: AppModel, entry: ObjectEntry, bucket?: string): Promise<void> {
  const location = locationOf(model, bucket === undefined ? undefined : { bucket, prefix: "" });
  if (location === null) {
    return;
  }
  if (isFolderEntry(entry)) {
    await model.browser.openLocation(location.bucket, entry.key);
    return;
  }
  try {
    await model.api.open_object({ account_id: location.accountId, bucket: location.bucket, key: entry.key });
    model.status.info(tr("Opening {key}.", { key: entry.key }));
  } catch (error) {
    model.status.error(error, { notify: tr("The object could not be opened") });
  }
}

/**
 * Opens the properties of the subject (default: the selection): one object or folder, or several objects to
 * edit together, on the given tab.
 */
export function showProperties(
  model: AppModel,
  subject: Subject | null = selectionSubject(model),
  tab?: ObjectPropertiesTab,
): void {
  if (subject !== null && subject.entries.length > 0) {
    model.dialogs.open({
      kind: "properties",
      accountId: subject.accountId,
      bucket: subject.bucket,
      entries: [...subject.entries],
      ...(tab === undefined ? {} : { tab }),
    });
  }
}

export function shareSelected(model: AppModel, subject: Subject | null = selectionSubject(model)): void {
  const entry = subject?.entries[0];
  if (subject !== null && subject.entries.length === 1 && entry !== undefined && !isFolderEntry(entry)) {
    model.dialogs.open({ kind: "share", accountId: subject.accountId, bucket: subject.bucket, key: entry.key });
  }
}

export function selectAllEntries(model: AppModel): void {
  model.browser.setSelection(selectAll(model.browser.state.visibleKeys));
}

export function invertSelectedEntries(model: AppModel): void {
  const { selection, visibleKeys } = model.browser.state;
  model.browser.setSelection(invert(selection, visibleKeys));
}

/** Moves keyboard focus to the filter box above the object list. */
export function focusSearch(): void {
  document.getElementById(SEARCH_INPUT_ID)?.focus();
}

export async function toggleFlatBrowsing(model: AppModel): Promise<void> {
  try {
    await model.settings.update((draft) => {
      draft.interface.flat_browsing = !draft.interface.flat_browsing;
    });
  } catch (error) {
    model.status.error(error, { notify: tr("The setting could not be saved") });
  }
}

export async function togglePanel(model: AppModel, panel: "transfers" | "log"): Promise<void> {
  try {
    await model.settings.update((draft) => {
      if (panel === "transfers") {
        draft.interface.show_transfer_panel = !draft.interface.show_transfer_panel;
      } else {
        draft.interface.show_log_panel = !draft.interface.show_log_panel;
      }
    });
  } catch (error) {
    model.status.error(error, { notify: tr("The setting could not be saved") });
  }
}
