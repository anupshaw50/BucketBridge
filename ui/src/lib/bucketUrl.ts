/** Browser URL of a bucket: virtual-hosted style on AWS, path style on custom endpoints unless the account disables it. */
export function bucketUrl(
  account: { endpoint: string | null; path_style: boolean },
  bucket: string,
  region: string | null,
): string {
  if (account.endpoint === null) {
    const where = region ?? "us-east-1";
    // A dot in the name would not match the wildcard certificate of the virtual-hosted host name.
    return bucket.includes(".") ? `https://s3.${where}.amazonaws.com/${bucket}` : `https://${bucket}.s3.${where}.amazonaws.com/`;
  }
  const base = account.endpoint.replace(/\/+$/, "");
  if (account.path_style) {
    return `${base}/${bucket}`;
  }
  const host = base.replace(/^https?:\/\//, "");
  const scheme = base.startsWith("http://") ? "http" : "https";
  return `${scheme}://${bucket}.${host}/`;
}
