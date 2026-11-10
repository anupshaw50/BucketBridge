// Rules for the default encryption and the tags of a bucket. The limits are those of
// bucketbridge_core::bucketprops (and S3); the mock backend applies the same rules, and the
// real backend re-checks everything.

import { tr } from "../i18n";
import { type Row, validateTag } from "./objectMeta";

export const MAX_BUCKET_TAGS = 50;
const MAX_KMS_KEY_ID_LENGTH = 2048;

/** A bucket's tag list: each tag valid, no duplicate keys, at most 50. */
export function validateBucketTagRows(rows: readonly Row[]): string | null {
  const seen = new Set<string>();
  for (const row of rows) {
    const problem = validateTag(row.key, row.value);
    if (problem !== null) {
      return problem;
    }
    if (seen.has(row.key)) {
      return tr("The tag \"{key}\" appears twice.", { key: row.key });
    }
    seen.add(row.key);
  }
  if (rows.length > MAX_BUCKET_TAGS) {
    return tr("A bucket can have at most {n} tags.", { n: MAX_BUCKET_TAGS });
  }
  return null;
}

/** A KMS key ID, key ARN, alias name or alias ARN; empty means the AWS managed key. `null` when valid. */
export function validateKmsKeyId(text: string): string | null {
  if (text.length === 0) {
    return null;
  }
  if (text.trim().length === 0 || /\s/u.test(text)) {
    return tr("The KMS key must be a key ID, key ARN, alias name or alias ARN without spaces.");
  }
  if (text.length > MAX_KMS_KEY_ID_LENGTH) {
    return tr("The KMS key is longer than {n} characters.", { n: MAX_KMS_KEY_ID_LENGTH });
  }
  return null;
}
