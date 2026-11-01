import type { AppModel } from "../../state/model";
import { type Place, openLocation } from "../objects/actions";

/** Opens the export dialog for a folder (default: the folder shown in the object list). */
export function openExportDialog(model: AppModel, at?: Place): void {
  const accountId = model.session.store.getState().activeAccountId;
  const location = at !== undefined && accountId !== null ? { accountId, ...at } : openLocation(model);
  if (location !== null) {
    model.dialogs.open({ kind: "export", ...location });
  }
}
