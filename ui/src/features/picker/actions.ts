import { isFolderEntry } from "../../lib/objects";
import type { AppModel } from "../../state/model";
import type { Subject } from "../objects/actions";
import type { TransferMode } from "./problem";

/** Opens the destination picker for the entries of a subject. */
export function chooseDestination(model: AppModel, mode: TransferMode, subject: Subject | null): void {
  if (subject === null || subject.entries.length === 0) {
    return;
  }
  model.dialogs.open({
    kind: "destination-picker",
    mode,
    accountId: subject.accountId,
    bucket: subject.bucket,
    keys: subject.entries.map((entry) => entry.key),
  });
}

/** The subject when it is a single folder, for the "all files" actions. */
export function folderSubject(subject: Subject | null): Subject | null {
  const [entry] = subject?.entries ?? [];
  return subject !== null && subject.entries.length === 1 && entry !== undefined && isFolderEntry(entry) ? subject : null;
}
