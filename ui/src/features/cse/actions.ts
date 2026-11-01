import { isFolderEntry } from "../../lib/objects";
import type { AppModel } from "../../state/model";
import { type Subject, selectionSubject } from "../objects/actions";

/** Opens the Encrypt or Decrypt dialog for the selected objects (folders are skipped). */
export function showEncryption(
  model: AppModel,
  mode: "encrypt" | "decrypt",
  subject: Subject | null = selectionSubject(model),
): void {
  if (subject === null) {
    return;
  }
  const keys = subject.entries.filter((entry) => !isFolderEntry(entry)).map((entry) => entry.key);
  if (keys.length > 0) {
    model.dialogs.open({ kind: "encryption", mode, accountId: subject.accountId, bucket: subject.bucket, keys });
  }
}
