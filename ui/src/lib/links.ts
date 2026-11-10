// Links for sharing: a public link on a CloudFront domain or a custom host name, and HTML and Markdown snippets.
// Pure functions; nothing here talks to a server.

import { baseName } from "./s3keys";

const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp", "svg", "avif", "bmp", "ico"];

/** Whether the key names an image, by its extension. */
export function isImageKey(key: string): boolean {
  const dot = key.lastIndexOf(".");
  return dot !== -1 && IMAGE_EXTENSIONS.includes(key.slice(dot + 1).toLowerCase());
}

/** Percent-encodes a key for a URL path: each segment is encoded, the slashes stay. */
export function encodeKeyPath(key: string): string {
  return key
    .split("/")
    .map((segment) => encodeURIComponent(segment).replace(/[!'()*]/g, (char) => `%${char.charCodeAt(0).toString(16).toUpperCase()}`))
    .join("/");
}

export type HostCheck = { ok: true; host: string } | { ok: false; problem: "empty" | "invalid" };

const LABEL = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/i;

/**
 * Reads the host of a public link: a domain name with an optional port, such as `d111111abcdef8.cloudfront.net` or
 * `cdn.example.com`. A pasted `https://` and a trailing slash are dropped; a path, a query, a user name, a space or a
 * label that is not a host name is `invalid`.
 */
export function checkHost(text: string): HostCheck {
  let value = text.trim();
  if (value.length === 0) {
    return { ok: false, problem: "empty" };
  }
  value = value.replace(/^https:\/\//i, "").replace(/\/+$/, "");
  const match = /^([^:/?#@\s]+)(?::(\d{1,5}))?$/.exec(value);
  if (match === null) {
    return { ok: false, problem: "invalid" };
  }
  const [, name = "", port] = match;
  const labels = name.split(".");
  if (name.length > 253 || labels.some((label) => !LABEL.test(label))) {
    return { ok: false, problem: "invalid" };
  }
  if (port !== undefined && (Number(port) < 1 || Number(port) > 65_535)) {
    return { ok: false, problem: "invalid" };
  }
  return { ok: true, host: port === undefined ? name.toLowerCase() : `${name.toLowerCase()}:${port}` };
}

/** `https://<host>/<key>`: the link of an object served by a CloudFront distribution or a custom host name. */
export function publicLink(host: string, key: string): string {
  return `https://${host}/${encodeKeyPath(key)}`;
}

function escapeHtml(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

function escapeMarkdownText(text: string): string {
  return text.replace(/[\\[\]]/g, (char) => `\\${char}`);
}

function markdownUrl(url: string): string {
  return url.replace(/\(/g, "%28").replace(/\)/g, "%29").replace(/\s/g, "%20");
}

/** The HTML for a link: an `<img>` for an image key, an `<a>` with the file name otherwise. */
export function htmlSnippet(url: string, key: string): string {
  const name = baseName(key);
  return isImageKey(key)
    ? `<img src="${escapeHtml(url)}" alt="${escapeHtml(name)}">`
    : `<a href="${escapeHtml(url)}">${escapeHtml(name)}</a>`;
}

/** The Markdown for a link: `![name](url)` for an image key, `[name](url)` otherwise. */
export function markdownSnippet(url: string, key: string): string {
  const name = escapeMarkdownText(baseName(key));
  return `${isImageKey(key) ? "!" : ""}[${name}](${markdownUrl(url)})`;
}
