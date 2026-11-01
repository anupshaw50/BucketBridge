import { isFolderEntry } from "../../lib/objects";
import type { AppModel } from "../../state/model";
import { type Place, openLocation, type Subject, selectionSubject } from "../objects/actions";

/** Opens the preview of an object (default: the selected one; a folder or several rows have none). */
export function previewSelected(model: AppModel, subject: Subject | null = selectionSubject(model)): void {
  const entry = subject?.entries[0];
  if (subject !== null && subject.entries.length === 1 && entry !== undefined && !isFolderEntry(entry)) {
    model.dialogs.open({
      kind: "preview",
      accountId: subject.accountId,
      bucket: subject.bucket,
      key: entry.key,
      versionId: null,
    });
  }
}

/**
 * Opens the versions of one object or folder (default: the single selected row), or, with no single row,
 * of the folder `at` (default: the open folder).
 */
export function showVersions(model: AppModel, subject: Subject | null = selectionSubject(model), at?: Place): void {
  const entry = subject?.entries[0];
  if (subject !== null && subject.entries.length === 1 && entry !== undefined) {
    model.dialogs.open({
      kind: "versions",
      accountId: subject.accountId,
      bucket: subject.bucket,
      prefix: entry.key,
      exact: !isFolderEntry(entry),
    });
    return;
  }
  const accountId = subject?.accountId ?? model.session.store.getState().activeAccountId;
  const location = at !== undefined && accountId !== null ? { accountId, ...at } : openLocation(model);
  if (location !== null) {
    model.dialogs.open({ kind: "versions", ...location, exact: false });
  }
}
