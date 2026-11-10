import { tr } from "../i18n";
import { isAccessPointArn, splitBucketAndKey } from "./accessPoint";
import { normalizePrefixInput, s3Uri } from "./s3keys";

export type AddressResult = { ok: true; bucket: string; prefix: string } | { ok: false; message: string };

/**
 * Reads a typed or pasted location: `s3://bucket/some/folder/` or `bucket/some/folder`; the bucket may be an
 * access point ARN (`s3://arn:aws:s3:us-west-2:123456789012:accesspoint/name/folder/`). A path without
 * a trailing slash is taken as a folder. The bucket name is only checked for characters it can contain
 * (providers differ in their naming rules); whether the account can see it is found out by listing it.
 */
export function parseAddress(text: string): AddressResult {
  let rest = text.trim();
  if (rest === "") {
    return { ok: false, message: tr("Enter a path such as s3://bucket/folder/.") };
  }
  const scheme = /^([a-z][a-z0-9+.-]*):\/\//i.exec(rest);
  if (scheme !== null) {
    if (scheme[1]?.toLowerCase() !== "s3") {
      return { ok: false, message: tr("Only s3:// paths can be opened here.") };
    }
    rest = rest.slice(scheme[0].length);
  } else {
    rest = rest.replace(/^\/+/, "");
  }
  const [bucket, prefix] = splitBucketAndKey(rest);
  if (bucket === "") {
    return { ok: false, message: tr("The path has no bucket name. Use s3://bucket/folder/.") };
  }
  if (!isAccessPointArn(bucket) && !/^[A-Za-z0-9][A-Za-z0-9._-]{0,254}$/.test(bucket)) {
    return { ok: false, message: tr("\"{bucket}\" is not a valid bucket name.", { bucket }) };
  }
  if (new TextEncoder().encode(prefix).length > 1024) {
    return { ok: false, message: tr("The folder path is longer than 1,024 bytes.") };
  }
  return { ok: true, bucket, prefix: normalizePrefixInput(prefix) };
}

/** The text the address bar shows for a location. */
export function addressText(bucket: string, prefix: string): string {
  return s3Uri(bucket, prefix);
}
