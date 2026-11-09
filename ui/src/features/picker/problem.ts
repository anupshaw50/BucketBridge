import { baseName, collidingNames, destinationKey, isFolderKey, pasteProblem, sameNameMessage } from "../../lib/s3keys";
import { tr } from "../../i18n";

/** What the picker is doing with the sources. */
export type TransferMode = "copy" | "move";

/**
 * Why `keys` of `sourceBucket` cannot go to the chosen destination, or null.
 * A folder can never go into itself or one of its own subfolders. With
 * `sameAccount` false the destination bucket belongs to another account, so
 * a bucket of the same name is another bucket and only name clashes matter.
 */
export function destinationProblem(
  mode: TransferMode,
  sourceBucket: string,
  keys: readonly string[],
  destinationBucket: string | null,
  destinationPrefix: string,
  sameAccount = true,
): string | null {
  if (destinationBucket === null) {
    return tr("Choose a bucket first.");
  }
  if (!sameAccount) {
    if (keys.length === 0) {
      return tr("Nothing is selected.");
    }
    const [first] = collidingNames(keys.map((key) => destinationKey(key, destinationPrefix)));
    return first === undefined ? null : sameNameMessage(baseName(first), mode === "move" ? tr("be moved") : tr("be copied"));
  }
  if (sourceBucket === destinationBucket) {
    for (const key of keys) {
      if (isFolderKey(key) && destinationPrefix.startsWith(key)) {
        return mode === "move"
          ? tr("A folder cannot be moved into itself or one of its subfolders.")
          : tr("A folder cannot be copied into itself or one of its subfolders.");
      }
    }
  }
  return pasteProblem(sourceBucket, keys, destinationBucket, destinationPrefix);
}
