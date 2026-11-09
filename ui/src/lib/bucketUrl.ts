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

export type UrlStyle = "virtual" | "path";

/** Percent-encodes an object key for a URL path, keeping the slashes. */
export function encodeKeyPath(key: string): string {
  return key
    .split("/")
    .map((part) => encodeURIComponent(part).replace(/[!'()*]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`))
    .join("/");
}

/**
 * URL of a bucket or one of its keys in the virtual-hosted or the path style. AWS accounts use the
 * regional host; accounts with a custom endpoint use that endpoint. An empty key gives the bucket URL.
 */
export function styledUrl(
  account: { endpoint: string | null; path_style: boolean },
  bucket: string,
  region: string | null,
  key: string,
  style: UrlStyle,
): string {
  const tail = key === "" ? "" : `/${encodeKeyPath(key)}`;
  if (account.endpoint === null) {
    const where = region ?? "us-east-1";
    return style === "virtual"
      ? `https://${bucket}.s3.${where}.amazonaws.com${tail || "/"}`
      : `https://s3.${where}.amazonaws.com/${bucket}${tail}`;
  }
  const base = account.endpoint.replace(/\/+$/, "");
  if (style === "path") {
    return `${base}/${bucket}${tail}`;
  }
  const host = base.replace(/^https?:\/\//, "");
  const scheme = base.startsWith("http://") ? "http" : "https";
  return `${scheme}://${bucket}.${host}${tail || "/"}`;
}

/** Whether the virtual-hosted style can work for the account (a custom endpoint set to path style cannot). */
export function supportsVirtualHosted(account: { endpoint: string | null; path_style: boolean }): boolean {
  return account.endpoint === null || !account.path_style;
}
