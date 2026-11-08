import type { Bookmark } from "../../api/types";
import type { AppModel } from "../../state/model";
import { tr } from "../../i18n";

/** Opens the dialog that bookmarks a location of an account. */
export function showAddBookmark(model: AppModel, accountId: string, bucket: string, prefix: string): void {
  model.dialogs.open({ kind: "bookmark", accountId, bucket, prefix });
}

/** Opens the dialog that renames a bookmark. */
export function showRenameBookmark(model: AppModel, bookmark: Bookmark): void {
  model.dialogs.open({
    kind: "bookmark",
    accountId: bookmark.account_id,
    bucket: bookmark.bucket,
    prefix: bookmark.prefix,
    bookmarkId: bookmark.id,
    label: bookmark.label,
  });
}

/** Removes a bookmark after a confirmation. Only the shortcut goes; the bucket and its objects are not touched. */
export async function removeBookmark(model: AppModel, bookmark: Bookmark): Promise<void> {
  const confirmed = await model.dialogs.confirm({
    title: tr("Delete bookmark"),
    message: tr(
      "Delete the bookmark \"{label}\"?\n\nOnly the shortcut is removed; the bucket and its objects are not changed.",
      { label: bookmark.label },
    ),
    confirmLabel: tr("Delete"),
  });
  if (!confirmed) {
    return;
  }
  try {
    await model.bookmarks.remove(bookmark.id);
    model.status.info(tr("Bookmark deleted."));
  } catch (error) {
    model.status.error(error, { notify: tr("The bookmark could not be deleted") });
  }
}

/** Opens another main window, at the given location when there is one. */
export async function openInNewWindow(
  model: AppModel,
  at: { accountId: string; bucket: string; prefix: string } | null,
): Promise<void> {
  await model.api.open_new_window({
    location: at === null ? null : { account_id: at.accountId, bucket: at.bucket, prefix: at.prefix },
  });
}
