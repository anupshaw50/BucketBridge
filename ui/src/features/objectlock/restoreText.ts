import type { ArchiveState } from "../../api/types";
import { tr } from "../../i18n";

/** Text for the restore state of an archived object. */
export function restoreText(state: ArchiveState): string {
  if (state.restore === null) {
    return state.archived
      ? tr("Archived ({class}), not restored.", { class: state.storage_class })
      : tr("Not archived ({class}).", { class: state.storage_class });
  }
  if (state.restore.in_progress) {
    return tr("A restore is in progress.");
  }
  return state.restore.expires === null
    ? tr("Restored.")
    : tr("Restored until {time}.", { time: state.restore.expires });
}
