import { isFolderKey } from "../../lib/s3keys";

export type ExpiryUnit = "seconds" | "minutes" | "hours" | "days";

export const UNIT_SECONDS: Record<ExpiryUnit, number> = { seconds: 1, minutes: 60, hours: 3600, days: 86_400 };

/** The longest lifetime of a presigned link: 7 days. */
export const MAX_PRESIGN_SECONDS = 7 * 86_400;

/** The key problem of the first wizard step, or null when the key names an object. */
export function keyProblem(key: string): string | null {
  if (key.trim().length === 0) {
    return "Enter the key of the object to share.";
  }
  if (isFolderKey(key)) {
    return "Choose an object, not a folder.";
  }
  return null;
}
