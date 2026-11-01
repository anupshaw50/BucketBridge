// Rules and helpers for the object metadata editor. The limits are those of
// bucketbridge_core::objectmeta (and S3); the mock backend applies the same
// rules, and the real backend re-checks everything.

import type { CannedAcl, HeaderChange, StorageClass } from "../api/types";

export const MAX_TAGS = 10;
export const MAX_TAG_KEY_CHARS = 128;
export const MAX_TAG_VALUE_CHARS = 256;
export const MAX_USER_META_BYTES = 2048;
export const MAX_HEADER_VALUE_BYTES = 2048;

export const STORAGE_CLASSES: readonly { value: StorageClass; label: string }[] = [
  { value: "STANDARD", label: "Standard" },
  { value: "STANDARD_IA", label: "Standard-IA (infrequent access)" },
  { value: "ONEZONE_IA", label: "One Zone-IA" },
  { value: "GLACIER_IR", label: "Glacier Instant Retrieval" },
  { value: "GLACIER", label: "Glacier Flexible Retrieval (archive)" },
  { value: "DEEP_ARCHIVE", label: "Glacier Deep Archive (archive)" },
  { value: "INTELLIGENT_TIERING", label: "Intelligent-Tiering" },
  { value: "REDUCED_REDUNDANCY", label: "Reduced redundancy (legacy)" },
];

export const CANNED_ACLS: readonly { value: CannedAcl; label: string }[] = [
  { value: "private", label: "Private" },
  { value: "public-read", label: "Public read" },
  { value: "public-read-write", label: "Public read and write" },
  { value: "authenticated-read", label: "Authenticated read (any AWS account)" },
  { value: "bucket-owner-read", label: "Bucket owner read" },
  { value: "bucket-owner-full-control", label: "Bucket owner full control" },
];

export function isStorageClass(value: string | null): value is StorageClass {
  return STORAGE_CLASSES.some((item) => item.value === value);
}

export function isCannedAcl(value: string | null): value is CannedAcl {
  return CANNED_ACLS.some((item) => item.value === value);
}

/** Classes whose data cannot be read until it is restored. */
export function isArchiveClass(value: string | null): boolean {
  return value === "GLACIER" || value === "DEEP_ARCHIVE";
}

export interface Row {
  key: string;
  value: string;
}

const TAG_TEXT = /^[\p{L}\p{N} +\-=._:/@]*$/u;

function tagTextError(what: string, text: string, max: number): string | null {
  if ([...text].length > max) {
    return `The ${what} is longer than ${max} characters.`;
  }
  if (!TAG_TEXT.test(text)) {
    return `The ${what} may contain letters, digits, spaces and + - = . _ : / @ only.`;
  }
  return null;
}

/** One tag against the S3 rules; `null` when it is valid. */
export function validateTag(key: string, value: string): string | null {
  if (key.length === 0) {
    return "A tag key cannot be empty.";
  }
  if (key.toLowerCase().startsWith("aws:")) {
    return 'Tag keys cannot start with "aws:".';
  }
  return tagTextError("tag key", key, MAX_TAG_KEY_CHARS) ?? tagTextError("tag value", value, MAX_TAG_VALUE_CHARS);
}

/** A list of tags: each valid, no duplicate keys, at most 10. */
export function validateTagRows(rows: readonly Row[]): string | null {
  const seen = new Set<string>();
  for (const row of rows) {
    const problem = validateTag(row.key, row.value);
    if (problem !== null) {
      return problem;
    }
    if (seen.has(row.key)) {
      return `The tag "${row.key}" appears twice.`;
    }
    seen.add(row.key);
  }
  if (rows.length > MAX_TAGS) {
    return `An object can have at most ${MAX_TAGS} tags.`;
  }
  return null;
}

/** Visible ASCII only (S3 headers are US-ASCII), no control characters. */
export function validateHeaderValue(name: string, value: string): string | null {
  if (value.trim().length === 0) {
    return `${name} is empty; remove the header instead.`;
  }
  if (new TextEncoder().encode(value).length > MAX_HEADER_VALUE_BYTES) {
    return `${name} is longer than ${MAX_HEADER_VALUE_BYTES} bytes.`;
  }
  if (!/^[\x20-\x7e]*$/.test(value)) {
    return `${name} can contain printable ASCII characters only.`;
  }
  return null;
}

const TOKEN = /^[A-Za-z0-9!#$%&'*+\-.^_`|~]+$/;

export function validateMediaType(value: string): string | null {
  const media = (value.split(";")[0] ?? "").trim();
  const [type, subtype, ...rest] = media.split("/");
  if (type === undefined || subtype === undefined || rest.length > 0 || !TOKEN.test(type) || !TOKEN.test(subtype)) {
    return "Content-Type must look like type/subtype, for example text/plain; charset=utf-8.";
  }
  return null;
}

export function validateMetaKey(key: string): string | null {
  if (key.length === 0) {
    return "A metadata key cannot be empty.";
  }
  if (!TOKEN.test(key)) {
    return `The metadata key "${key}" may contain letters, digits and ! # $ % & ' * + - . ^ _ \` | ~ only.`;
  }
  return null;
}

/** The UTF-8 size S3 counts against the 2 KB limit. */
export function metaBytes(meta: Readonly<Record<string, string>>): number {
  const encoder = new TextEncoder();
  return Object.entries(meta).reduce((sum, [key, value]) => sum + encoder.encode(key).length + encoder.encode(value).length, 0);
}

export function rowsToRecord(rows: readonly Row[], lowercaseKeys = false): Record<string, string> {
  return Object.fromEntries(rows.map((row) => [lowercaseKeys ? row.key.toLowerCase() : row.key, row.value]));
}

export function recordToRows(record: Readonly<Record<string, string>>): Row[] {
  return Object.entries(record)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([key, value]) => ({ key, value }));
}

/** User metadata rows: valid keys and values, unique keys (not case sensitive), at most 2048 bytes. */
export function validateMetaRows(rows: readonly Row[]): string | null {
  const seen = new Set<string>();
  for (const row of rows) {
    const problem = validateMetaKey(row.key) ?? validateHeaderValue(`The value of "${row.key}"`, row.value);
    if (problem !== null) {
      return problem;
    }
    const lower = row.key.toLowerCase();
    if (seen.has(lower)) {
      return `The metadata key "${row.key}" appears twice (keys are not case sensitive).`;
    }
    seen.add(lower);
  }
  const size = metaBytes(rowsToRecord(rows, true));
  if (size > MAX_USER_META_BYTES) {
    return `User metadata is ${size} bytes; S3 allows at most ${MAX_USER_META_BYTES} for all keys and values together.`;
  }
  return null;
}

/**
 * The patch for one standard header. With `batch` the field starts empty and
 * an empty value keeps each object's header; otherwise the field starts with
 * the object's value and clearing it removes the header.
 */
export function headerChange(initial: string, value: string, remove: boolean, batch: boolean): HeaderChange {
  if (batch) {
    if (remove) {
      return { action: "remove" };
    }
    return value.trim().length > 0 ? { action: "set", value } : { action: "keep" };
  }
  if (value === initial) {
    return { action: "keep" };
  }
  if (value.trim().length === 0) {
    return initial.length > 0 ? { action: "remove" } : { action: "keep" };
  }
  return { action: "set", value };
}

/** What a user metadata editor changes compared to what the object has. */
export function metaPatch(initial: Readonly<Record<string, string>>, rows: readonly Row[]): { set: Record<string, string>; remove: string[] } {
  const wanted = rowsToRecord(rows, true);
  const set = Object.fromEntries(Object.entries(wanted).filter(([key, value]) => initial[key] !== value));
  const remove = Object.keys(initial).filter((key) => !(key in wanted));
  return { set, remove };
}

// ---- customer-provided keys (SSE-C) ----

/** The key as 64 lowercase hex digits, from hex or base64 text; `null` when it is neither. */
export function normalizeCustomerKey(text: string): string | null {
  const trimmed = text.trim();
  if (/^[0-9a-fA-F]{64}$/.test(trimmed)) {
    return trimmed.toLowerCase();
  }
  if (/^[A-Za-z0-9+/]{43}=?$/.test(trimmed)) {
    try {
      const bytes = Uint8Array.from(atob(trimmed.padEnd(44, "=")), (c) => c.charCodeAt(0));
      if (bytes.length === 32) {
        return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
      }
    } catch {
      return null;
    }
  }
  return null;
}

export function validateCustomerKey(text: string): string | null {
  if (text.trim().length === 0) {
    return "Enter the key.";
  }
  return normalizeCustomerKey(text) === null
    ? "The key must be 32 bytes: 64 hexadecimal digits, or 44 base64 characters."
    : null;
}

/** A fresh random key, as base64 (what the key field takes). */
export function generateCustomerKey(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}
