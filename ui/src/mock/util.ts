// Small helpers for the mock backend.

/** RFC 4122 version 4 UUID from crypto.getRandomValues (works in every test environment). */
export function uuid(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** Orders strings by Unicode code point, which matches S3's UTF-8 binary key order. */
export function compareKeys(a: string, b: string): number {
  if (a === b) {
    return 0;
  }
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    const ca = a.codePointAt(i) ?? 0;
    const cb = b.codePointAt(j) ?? 0;
    if (ca !== cb) {
      return ca < cb ? -1 : 1;
    }
    i += ca > 0xffff ? 2 : 1;
    j += cb > 0xffff ? 2 : 1;
  }
  if (i < a.length) {
    return 1;
  }
  return j < b.length ? -1 : 0;
}

/** Deterministic pseudo-random bytes derived from a seed string (FNV-1a + xorshift). */
export function seededBytes(seed: string, length: number): Uint8Array {
  let hash = 0x811c9dc5;
  for (let index = 0; index < seed.length; index += 1) {
    hash ^= seed.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  const out = new Uint8Array(length);
  let state = hash || 1;
  for (let index = 0; index < length; index += 1) {
    state ^= state << 13;
    state >>>= 0;
    state ^= state >>> 17;
    state ^= state << 5;
    state >>>= 0;
    out[index] = state & 0xff;
  }
  return out;
}

export function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function toBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

export function nowIso(): string {
  return new Date().toISOString();
}

export function isoMinusDays(days: number, base = Date.now()): string {
  return new Date(base - days * 86_400_000).toISOString();
}

export function clone<T>(value: T): T {
  return structuredClone(value);
}

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export const KiB = 1024;
export const MiB = 1024 * 1024;
export const GiB = 1024 * 1024 * 1024;
