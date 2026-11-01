// Input validation shared by dialogs (and by the mock backend, which applies
// the same rules the real backend is expected to apply).

const RESERVED_BUCKET_PREFIXES = ["xn--", "sthree-", "amzn-s3-demo-"];
const RESERVED_BUCKET_SUFFIXES = ["-s3alias", "--ol-s3", ".mrap", "--x-s3", "--table-s3"];

/** S3 general purpose bucket naming rules. Returns an error message or `null`. */
export function validateBucketName(name: string): string | null {
  if (name.length === 0) {
    return "Enter a bucket name.";
  }
  if (name.length < 3 || name.length > 63) {
    return "Bucket names must be between 3 and 63 characters long.";
  }
  if (!/^[a-z0-9.-]+$/.test(name)) {
    return "Use only lowercase letters, digits, dots (.) and hyphens (-).";
  }
  if (!/^[a-z0-9]/.test(name) || !/[a-z0-9]$/.test(name)) {
    return "Bucket names must begin and end with a letter or digit.";
  }
  if (name.includes("..")) {
    return "Bucket names cannot contain two adjacent dots.";
  }
  if (name.includes(".-") || name.includes("-.")) {
    return "A dot cannot be next to a hyphen.";
  }
  if (/^\d{1,3}(\.\d{1,3}){3}$/.test(name)) {
    return "Bucket names cannot look like an IP address.";
  }
  const reservedPrefix = RESERVED_BUCKET_PREFIXES.find((prefix) => name.startsWith(prefix));
  if (reservedPrefix) {
    return `Bucket names cannot start with "${reservedPrefix}".`;
  }
  const reservedSuffix = RESERVED_BUCKET_SUFFIXES.find((suffix) => name.endsWith(suffix));
  if (reservedSuffix) {
    return `Bucket names cannot end with "${reservedSuffix}".`;
  }
  return null;
}

/** Region names: lowercase letters, digits and hyphens ("us-east-1", or "auto" for some providers). */
export function validateRegion(region: string): string | null {
  if (region.length === 0) {
    return "Enter a region.";
  }
  if (region.length > 64 || !/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(region)) {
    return "Use lowercase letters, digits and hyphens, for example us-east-1.";
  }
  return null;
}

/** Custom endpoint: an absolute http(s) URL without credentials, query or fragment. */
export function validateEndpoint(endpoint: string): string | null {
  if (endpoint.length === 0) {
    return null;
  }
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch {
    return "Enter a full URL such as https://storage.example.com.";
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") {
    return "The endpoint must start with https:// or http://.";
  }
  if (url.username !== "" || url.password !== "") {
    return "Do not put credentials in the endpoint URL.";
  }
  if (url.search !== "" || url.hash !== "") {
    return "The endpoint cannot contain a query string or fragment.";
  }
  return null;
}

/** Host name or IPv4/IPv6 address for the proxy. */
export function validateHost(host: string): string | null {
  if (host.length === 0) {
    return "Enter the proxy host.";
  }
  if (/\s/.test(host) || host.includes("://") || host.includes("/")) {
    return "Enter a host name or IP address without a scheme or path.";
  }
  if (host.startsWith("[") && host.endsWith("]")) {
    return /^\[[0-9a-fA-F:.]+\]$/.test(host) ? null : "Enter a valid IPv6 address.";
  }
  if (host.includes(":")) {
    return /^[0-9a-fA-F:.]+$/.test(host) ? null : "Enter the port in the port field.";
  }
  if (!/^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*$/.test(host)) {
    return "Enter a valid host name or IP address.";
  }
  return null;
}

function validIpv6(value: string): boolean {
  const bare = value.startsWith("[") && value.endsWith("]") ? value.slice(1, -1) : value;
  if (!/^[0-9a-fA-F:]+$/.test(bare) || !bare.includes(":") || bare.includes(":::")) {
    return false;
  }
  const halves = bare.split("::");
  if (halves.length > 2) {
    return false;
  }
  const groups = halves.flatMap((half) => (half === "" ? [] : half.split(":")));
  if (groups.some((group) => group.length === 0 || group.length > 4)) {
    return false;
  }
  return halves.length === 2 ? groups.length <= 7 : groups.length === 8;
}

function validIpv4(value: string): boolean {
  const parts = value.split(".");
  return (
    parts.length === 4 &&
    parts.every((part) => /^\d{1,3}$/.test(part) && Number(part) <= 255 && String(Number(part)) === part)
  );
}

/** One proxy bypass entry: a host name, "*.suffix", an IPv4 or IPv6 address, or a CIDR range. */
export function validateBypassEntry(entry: string): string | null {
  if (entry.length === 0) {
    return "Remove empty entries.";
  }
  if (entry.includes("/")) {
    const [address = "", bits = "", ...rest] = entry.split("/");
    if (rest.length > 0 || !/^\d{1,3}$/.test(bits)) {
      return `"${entry}" is not a valid CIDR range.`;
    }
    const maskBits = Number(bits);
    if (validIpv4(address)) {
      return maskBits <= 32 ? null : `"${entry}" has a prefix length above 32.`;
    }
    if (validIpv6(address)) {
      return maskBits <= 128 ? null : `"${entry}" has a prefix length above 128.`;
    }
    return `"${entry}" is not a valid CIDR range.`;
  }
  const host = entry.startsWith("*.") ? entry.slice(2) : entry;
  if (validIpv4(host) || (entry === host && validIpv6(host))) {
    return null;
  }
  if (!/^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*$/.test(host)) {
    return `"${entry}" is not a host name, "*.suffix" pattern, IP address or CIDR range.`;
  }
  return null;
}

/** Splits the bypass text area (one entry per line or comma separated). */
export function parseBypassList(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
}
