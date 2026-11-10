/**
 * S3 access point ARNs, accepted wherever a bucket is chosen: `arn:aws:s3:us-west-2:123456789012:accesspoint/name`
 * (one region) or `arn:aws:s3::123456789012:accesspoint/alias.mrap` (a Multi-Region Access Point).
 * Mirrors `parse_access_point_arn` of the core.
 */
const ACCESS_POINT_ARN = /^arn:[a-z-]+:s3:([a-z0-9-]*):\d{12}:accesspoint\/[A-Za-z0-9.-]{1,68}$/;

export function isAccessPointArn(text: string): boolean {
  const match = ACCESS_POINT_ARN.exec(text);
  if (match === null) {
    return false;
  }
  // Only a Multi-Region Access Point has no region.
  return match[1] !== "" || text.endsWith(".mrap");
}

/** Splits the text after `s3://` into the bucket (a name or an access point ARN) and the key. */
export function splitBucketAndKey(rest: string): [string, string] {
  if (rest.startsWith("arn:")) {
    const marker = ":accesspoint/";
    const at = rest.indexOf(marker);
    if (at >= 0) {
      const nameStart = at + marker.length;
      const slash = rest.indexOf("/", nameStart);
      return slash === -1 ? [rest, ""] : [rest.slice(0, slash), rest.slice(slash + 1)];
    }
  }
  const slash = rest.indexOf("/");
  return slash === -1 ? [rest, ""] : [rest.slice(0, slash), rest.slice(slash + 1)];
}
