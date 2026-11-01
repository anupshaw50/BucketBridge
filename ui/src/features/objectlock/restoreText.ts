import type { ArchiveState } from "../../api/types";

/** Text for the restore state of an archived object. */
export function restoreText(state: ArchiveState): string {
  if (state.restore === null) {
    return state.archived ? `Archived (${state.storage_class}), not restored.` : `Not archived (${state.storage_class}).`;
  }
  if (state.restore.in_progress) {
    return "A restore is in progress.";
  }
  return `Restored${state.restore.expires === null ? "" : ` until ${state.restore.expires}`}.`;
}
