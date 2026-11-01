import { isFolderKey, pasteProblem } from "../../lib/s3keys";

/** What the picker is doing with the sources. */
export type TransferMode = "copy" | "move";

/**
 * Why `keys` of `sourceBucket` cannot go to the chosen destination, or null.
 * A folder can never go into itself or one of its own subfolders.
 */
export function destinationProblem(
  mode: TransferMode,
  sourceBucket: string,
  keys: readonly string[],
  destinationBucket: string | null,
  destinationPrefix: string,
): string | null {
  if (destinationBucket === null) {
    return "Choose a bucket first.";
  }
  if (sourceBucket === destinationBucket) {
    for (const key of keys) {
      if (isFolderKey(key) && destinationPrefix.startsWith(key)) {
        return `A folder cannot be ${mode === "move" ? "moved" : "copied"} into itself or one of its subfolders.`;
      }
    }
  }
  return pasteProblem(sourceBucket, keys, destinationBucket, destinationPrefix);
}
