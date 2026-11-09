import { BackendError } from "../api/errors";
import type {
  BbErrorCode,
  DeleteOutcome,
  ObjectPreview,
  ObjectVersion,
  PreviewKind,
  VersionFilter,
  VersionGroup,
  VersionGroupPage,
  VersionRef,
} from "../api/types";
import { isFolderKey } from "../lib/s3keys";
import { compareKeys, nowIso, toBase64 } from "./util";
import { makeObject, type MockBucket, type Provider } from "./world";

/** One version or delete marker of a key in the mock cloud. */
export interface MockVersionEntry {
  version_id: string;
  is_delete_marker: boolean;
  size: number;
  etag: string | null;
  last_modified: string;
  storage_class: string | null;
  /** Names the content: entries with the same seed hold the same bytes. */
  seed: string;
}

export const DEFAULT_PAGE_BYTES = 131_072;
export const MIN_PAGE_BYTES = 16;
export const MAX_PAGE_BYTES = 1_048_576;
export const MAX_IMAGE_BYTES = 8 * 1024 * 1024;

/** A 1x1 lossy WebP, so the mock has real WebP bytes to hand to the web view. */
/** A 1x1 GIF, so the mock has real GIF bytes (shown animated when they are). */
const GIF_BASE64 = "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

const WEBP_BASE64 = "UklGRiIAAABXRUJQVlA4IBYAAAAwAQCdASoBAAEADsD+JaQAA3AAAAAA";

const IMAGE_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/png",
  jpeg: "image/png",
  gif: "image/gif",
  bmp: "image/png",
  ico: "image/png",
  avif: "image/png",
  webp: "image/webp",
  svg: "image/svg+xml",
};
const TEXT_EXTENSIONS = new Set(["txt", "md", "log", "html", "css", "js", "xml", "yml", "yaml", "ini", "sql", "sh"]);

const TABLE_TYPES: Record<string, string> = {
  csv: "text/csv",
  tsv: "text/tab-separated-values",
  tab: "text/tab-separated-values",
  parquet: "application/vnd.apache.parquet",
  parq: "application/vnd.apache.parquet",
};
export const TABLE_ROWS_PER_PAGE = 200;
const PRETTY_XML = '<?xml version="1.0"?>\n<catalog>\n  <book id="1">\n    <title>First</title>\n    <price>9.50</price>\n  </book>\n  <book id="2"/>\n</catalog>';

/** Rows of a demo table: about one per 40 bytes, three columns. */
function tablePage(entry: MockVersionEntry, mediaType: string, extension: string, page: number): Partial<ObjectPreview> {
  const total = Math.max(1, Math.floor(entry.size / 40));
  const count = Math.max(1, Math.ceil(total / TABLE_ROWS_PER_PAGE));
  if (!Number.isInteger(page) || page < 0 || (page > 0 && page >= count)) {
    fail("usage", `page ${page} is past the end of the table`);
  }
  const first = page * TABLE_ROWS_PER_PAGE;
  const rows: (string | null)[][] = [];
  for (let n = first; n < Math.min(total, first + TABLE_ROWS_PER_PAGE); n++) {
    rows.push([String(n), n % 3 === 0 ? null : `region ${n % 7}`, (n * 1.25).toFixed(2)]);
  }
  return {
    media_type: mediaType,
    page,
    page_count: count,
    page_size: TABLE_ROWS_PER_PAGE,
    offset: first,
    length: rows.length,
    table: {
      columns: ["id", "region", extension === "parquet" ? "amount" : "amount, in USD"],
      rows,
      total_rows: total,
      truncated: false,
      columns_cut: false,
    },
  };
}

function fail(code: BbErrorCode, message: string): never {
  throw new BackendError(code, message);
}

function extensionOf(key: string): string {
  const name = key.slice(key.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  return dot === -1 ? "" : name.slice(dot + 1).toLowerCase();
}

function asciiSeed(seed: string): string {
  return seed.replace(/[^\x20-\x7e]/g, "_");
}

/** Line `n` of a generated text: exactly 64 bytes of ASCII, so any byte window can be cut from it. */
function textLine(seed: string, n: number): string {
  const label = `${asciiSeed(seed).slice(0, 24)} line ${n.toString().padStart(7, "0")} `;
  return `${(label + "lorem ipsum dolor sit amet ".repeat(4)).slice(0, 63)}\n`;
}

/** Record `n` of a generated JSON array: exactly 64 bytes. */
function jsonLine(seed: string, n: number): string {
  const head = `{"n":${n.toString().padStart(7, "0")},"s":"${asciiSeed(seed).padEnd(20, "_").slice(0, 20).replace(/"/g, "_")}","pad":"`;
  return `${head}${"x".repeat(60 - head.length)}"},\n`.slice(0, 64);
}

/** Bytes `start` to `end` (exclusive) of the text whose line `n` is `line(n)`. */
function byteWindow(line: (n: number) => string, start: number, end: number): string {
  let out = "";
  let position = start;
  while (position < end) {
    const n = Math.floor(position / 64);
    const from = position - n * 64;
    const take = Math.min(64 - from, end - position);
    out += line(n).slice(from, from + take);
    position += take;
  }
  return out;
}

/** A tiny RGB PNG (a gradient) built without compression, to show in the browser. */
export function gradientPngBase64(width = 96, height = 64): string {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k += 1) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    return c >>> 0;
  });
  const crc = (bytes: number[]) => {
    let c = 0xffffffff;
    for (const byte of bytes) {
      c = (crcTable[(c ^ byte) & 0xff] ?? 0) ^ (c >>> 8);
    }
    return (c ^ 0xffffffff) >>> 0;
  };
  const be32 = (value: number) => [(value >>> 24) & 255, (value >>> 16) & 255, (value >>> 8) & 255, value & 255];
  const chunk = (type: string, data: number[]) => {
    const body = [...Array.from(type, (ch) => ch.charCodeAt(0)), ...data];
    return [...be32(data.length), ...body, ...be32(crc(body))];
  };
  const raw: number[] = [];
  for (let y = 0; y < height; y += 1) {
    raw.push(0);
    for (let x = 0; x < width; x += 1) {
      raw.push(Math.floor((x * 255) / width), Math.floor((y * 255) / height), 160);
    }
  }
  // zlib with stored blocks: no compression, so no deflate code is needed.
  const stored: number[] = [0x78, 0x01];
  for (let at = 0; at < raw.length; at += 65_535) {
    const block = raw.slice(at, at + 65_535);
    const last = at + 65_535 >= raw.length ? 1 : 0;
    stored.push(last, block.length & 255, block.length >>> 8, ~block.length & 255, (~block.length >>> 8) & 255, ...block);
  }
  let a = 1;
  let b = 0;
  for (const byte of raw) {
    a = (a + byte) % 65_521;
    b = (b + a) % 65_521;
  }
  stored.push(...be32(((b << 16) | a) >>> 0));
  const png = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
    ...chunk("IHDR", [...be32(width), ...be32(height), 8, 2, 0, 0, 0]),
    ...chunk("IDAT", stored),
    ...chunk("IEND", []),
  ];
  return toBase64(Uint8Array.from(png));
}

/** Rejects filters that can never match anything, with the rules of the core. */
export function validateVersionFilter(filter: VersionFilter): void {
  if (!filter.show_versions && filter.delete_markers === "exclude") {
    fail("usage", "nothing would be listed: turn on versions or delete markers");
  }
  if (
    filter.modified_after !== null &&
    filter.modified_before !== null &&
    Date.parse(filter.modified_after) >= Date.parse(filter.modified_before)
  ) {
    fail("usage", "modified_after must be earlier than modified_before (the interval is empty)");
  }
  const least = Math.max((filter.count_greater_than ?? 0) + 1, filter.multiple_versions ? 2 : 1);
  if (filter.count_less_than !== null && filter.count_less_than <= least) {
    fail("usage", `no key can have at least ${least} and fewer than ${filter.count_less_than} versions`);
  }
}

function entryMatches(filter: VersionFilter, entry: MockVersionEntry): boolean {
  const kind = entry.is_delete_marker
    ? filter.delete_markers !== "exclude"
    : filter.show_versions && filter.delete_markers !== "only";
  if (!kind) {
    return false;
  }
  const time = Date.parse(entry.last_modified);
  return (
    (filter.modified_after === null || time >= Date.parse(filter.modified_after)) &&
    (filter.modified_before === null || time < Date.parse(filter.modified_before))
  );
}

function keyMatches(filter: VersionFilter, count: number): boolean {
  return (
    (!filter.multiple_versions || count >= 2) &&
    (filter.count_greater_than === null || count > filter.count_greater_than) &&
    (filter.count_less_than === null || count < filter.count_less_than)
  );
}

function encodeToken(key: string): string {
  return `v2:${encodeURIComponent(key)}`;
}

function decodeToken(token: string): string {
  if (!token.startsWith("v2:")) {
    fail("usage", "invalid continuation token for a version listing");
  }
  try {
    return decodeURIComponent(token.slice(3));
  } catch {
    fail("usage", "invalid continuation token for a version listing");
  }
}

function toVersion(key: string, entry: MockVersionEntry, latest: boolean): ObjectVersion {
  return {
    key,
    version_id: entry.version_id,
    is_latest: latest,
    is_delete_marker: entry.is_delete_marker,
    size: entry.size,
    etag: entry.etag,
    last_modified: entry.last_modified,
    storage_class: entry.storage_class,
  };
}

/**
 * The versions of the mock cloud. A key's history is recorded here, and
 * `entries` adds what happened to the live objects since (an overwrite is a new
 * version, a deletion a delete marker), so uploads, copies and deletes of the
 * rest of the mock need no hooks.
 */
export class MockVersions {
  private readonly history = new Map<string, Map<string, MockVersionEntry[]>>();
  private counter = 0;

  private nextId(): string {
    this.counter += 1;
    return `ver-${this.counter.toString().padStart(4, "0")}`;
  }

  private keyHistory(bucket: string, key: string): MockVersionEntry[] {
    let keys = this.history.get(bucket);
    if (keys === undefined) {
      keys = new Map();
      this.history.set(bucket, keys);
    }
    let list = keys.get(key);
    if (list === undefined) {
      list = [];
      keys.set(key, list);
    }
    return list;
  }

  /** Records older versions of a key, newest first. */
  recordOlder(bucket: string, key: string, older: readonly MockVersionEntry[]): void {
    this.keyHistory(bucket, key).push(...older);
  }

  /** Records that a key was deleted at `at`: a delete marker on top of what is recorded. */
  recordDeleted(bucket: string, key: string, at: string, id: string): void {
    this.keyHistory(bucket, key).unshift({
      version_id: id,
      is_delete_marker: true,
      size: 0,
      etag: null,
      last_modified: at,
      storage_class: null,
      seed: "",
    });
  }

  /** The entries of a key, newest first, including what happened to the live object. */
  entries(bucket: MockBucket, key: string): MockVersionEntry[] {
    const list = this.keyHistory(bucket.name, key);
    const current = bucket.objects.get(key);
    const head = list[0];
    if (current !== undefined) {
      if (head === undefined || head.is_delete_marker || head.etag !== current.etag) {
        list.unshift({
          version_id: this.nextId(),
          is_delete_marker: false,
          size: current.size,
          etag: current.etag,
          last_modified: current.last_modified,
          storage_class: current.storage_class,
          seed: `${key}:${current.etag}`,
        });
      }
    } else if (head !== undefined && !head.is_delete_marker) {
      list.unshift({
        version_id: this.nextId(),
        is_delete_marker: true,
        size: 0,
        etag: null,
        last_modified: nowIso(),
        storage_class: null,
        seed: "",
      });
    }
    return list;
  }

  /** The newest data version of a key made live again (after a restore or a purge). */
  private syncLive(bucket: MockBucket, key: string, list: MockVersionEntry[]): void {
    const head = list[0];
    if (head === undefined || head.is_delete_marker) {
      bucket.objects.delete(key);
      return;
    }
    bucket.objects.set(key, {
      ...makeObject(key, head.size, head.last_modified),
      etag: head.etag ?? "",
      storage_class: head.storage_class ?? "STANDARD",
    });
  }

  /** Every key under a prefix that has a live object or a recorded history, in key order. */
  keysOf(bucket: MockBucket, prefix: string): string[] {
    const keys = new Set<string>(bucket.objects.keys());
    for (const [key, list] of this.history.get(bucket.name) ?? []) {
      if (list.length > 0) {
        keys.add(key);
      }
    }
    return [...keys].filter((key) => key.startsWith(prefix)).sort(compareKeys);
  }

  list(
    bucket: MockBucket,
    args: { prefix: string; exact: boolean; filter: VersionFilter; token: string | null },
    pageSize: number,
  ): VersionGroupPage {
    validateVersionFilter(args.filter);
    if (args.exact && (args.prefix.length === 0 || isFolderKey(args.prefix))) {
      fail("usage", "an exact listing needs an object key");
    }
    const after = args.token === null ? null : decodeToken(args.token);
    const keys = new Set<string>(bucket.objects.keys());
    for (const [key, list] of this.history.get(bucket.name) ?? []) {
      if (list.length > 0) {
        keys.add(key);
      }
    }
    const wanted = [...keys]
      .filter((key) => (args.exact ? key === args.prefix : key.startsWith(args.prefix)))
      .filter((key) => after === null || compareKeys(key, after) > 0)
      .sort(compareKeys);
    const groups: VersionGroup[] = [];
    let scanned = 0;
    let last: string | null = null;
    let more = false;
    for (const key of wanted) {
      if (groups.length === pageSize) {
        more = true;
        break;
      }
      const entries = this.entries(bucket, key);
      scanned += entries.length;
      last = key;
      const kept = entries.filter((entry) => entryMatches(args.filter, entry));
      if (kept.length === 0 || !keyMatches(args.filter, kept.length)) {
        continue;
      }
      groups.push({
        key,
        versions: kept.map((entry) => toVersion(key, entry, entry === entries[0])),
        total: entries.length,
        matching: kept.length,
        is_deleted: entries[0]?.is_delete_marker ?? false,
        truncated: false,
      });
    }
    // The page ended because it was full: the token continues after the last key judged.
    return { groups, next_token: more && last !== null ? encodeToken(last) : null, scanned };
  }

  restore(bucket: MockBucket, key: string, versionId: string): { entry: MockVersionEntry; restoredFrom: MockVersionEntry } {
    if (versionId.length === 0) {
      fail("usage", "the version ID is empty");
    }
    const list = this.entries(bucket, key);
    const found = list.find((entry) => entry.version_id === versionId);
    if (found === undefined) {
      fail("not_found", `The specified version does not exist (NoSuchVersion): ${versionId}`);
    }
    if (found.is_delete_marker) {
      fail(
        "usage",
        `version ${versionId} of s3://${bucket.name}/${key} is a delete marker, which has no content to restore; remove the delete marker to bring the previous version back`,
      );
    }
    if (list[0] === found) {
      fail("conflict", `version ${versionId} of s3://${bucket.name}/${key} already is the current version`);
    }
    const copy: MockVersionEntry = { ...found, version_id: this.nextId(), last_modified: nowIso() };
    list.unshift(copy);
    this.syncLive(bucket, key, list);
    return { entry: copy, restoredFrom: found };
  }

  purge(bucket: MockBucket, refs: readonly VersionRef[]): DeleteOutcome {
    const seen = new Set<string>();
    let deleted = 0;
    for (const ref of refs) {
      if (ref.version_id.length === 0) {
        fail("usage", `the version ID of s3://${bucket.name}/${ref.key} is empty`);
      }
      const id = `${ref.key}\u0000${ref.version_id}`;
      if (seen.has(id)) {
        continue;
      }
      seen.add(id);
      const list = this.entries(bucket, ref.key);
      const at = list.findIndex((entry) => entry.version_id === ref.version_id);
      if (at !== -1) {
        list.splice(at, 1);
        this.syncLive(bucket, ref.key, list);
      }
      deleted += 1;
    }
    return { deleted, failures: [] };
  }

  /** The entry a version download or a preview reads. */
  find(bucket: MockBucket, key: string, versionId: string | null): MockVersionEntry {
    const list = this.entries(bucket, key);
    const entry = versionId === null ? list[0] : list.find((item) => item.version_id === versionId);
    if (entry === undefined || (versionId === null && entry.is_delete_marker)) {
      fail("not_found", versionId === null ? "The specified key does not exist (NoSuchKey)." : `The specified version does not exist (NoSuchVersion): ${versionId}`);
    }
    return entry;
  }

  preview(
    bucket: MockBucket,
    key: string,
    versionId: string | null,
    page: number,
    pageSize: number | null,
  ): ObjectPreview {
    if (key.length === 0 || isFolderKey(key)) {
      fail("usage", `s3://${bucket.name}/${key} is a folder; only objects can be previewed`);
    }
    const bytes = pageSize ?? DEFAULT_PAGE_BYTES;
    if (!Number.isInteger(bytes) || bytes < MIN_PAGE_BYTES || bytes > MAX_PAGE_BYTES) {
      fail("usage", `the preview page size must be between ${MIN_PAGE_BYTES} and ${MAX_PAGE_BYTES} bytes (got ${bytes})`);
    }
    const entry = this.find(bucket, key, versionId);
    if (entry.is_delete_marker) {
      fail("usage", `version ${versionId ?? ""} of s3://${bucket.name}/${key} is a delete marker; it has no content`);
    }
    const base: ObjectPreview = {
      key,
      version_id: entry.version_id,
      kind: "binary",
      size: entry.size,
      content_type: null,
      media_type: null,
      last_modified: entry.last_modified,
      etag: entry.etag,
      page: 0,
      page_count: 1,
      page_size: bytes,
      offset: 0,
      length: 0,
      text: null,
      formatted: false,
      image_base64: null,
      table: null,
    };
    const kind = (value: PreviewKind, rest: Partial<ObjectPreview> = {}): ObjectPreview => ({ ...base, kind: value, ...rest });
    if (entry.size === 0) {
      return kind("empty");
    }
    const extension = extensionOf(key);
    const imageType = IMAGE_TYPES[extension];
    if (imageType !== undefined) {
      if (entry.size > MAX_IMAGE_BYTES) {
        return kind("too_large", { media_type: imageType });
      }
      const image =
        extension === "webp"
          ? WEBP_BASE64
          : extension === "gif"
            ? GIF_BASE64
            : extension === "svg"
            ? toBase64(
                new TextEncoder().encode(
                  `<svg xmlns="http://www.w3.org/2000/svg" width="96" height="64"><rect width="96" height="64" fill="#2b7de9"/><text x="8" y="36" fill="#fff" font-size="14">${asciiSeed(entry.seed).slice(0, 8).replace(/[<>&]/g, "_")}</text></svg>`,
                ),
              )
            : gradientPngBase64();
      return kind("image", { media_type: imageType, length: entry.size, image_base64: image });
    }
    const tableType = TABLE_TYPES[extension];
    if (tableType !== undefined) {
      if (extension === "parquet" && entry.size > MAX_IMAGE_BYTES) {
        return kind("too_large", { media_type: tableType });
      }
      return kind("table", tablePage(entry, tableType, extension, page));
    }
    const isJson = extension === "json";
    if (!isJson && !TEXT_EXTENSIONS.has(extension)) {
      return kind("binary");
    }
    const count = Math.max(1, Math.ceil(entry.size / bytes));
    if (!Number.isInteger(page) || page < 0 || page >= count) {
      fail("usage", `page ${page} is past the end of s3://${bucket.name}/${key}`);
    }
    const offset = page * bytes;
    const length = Math.min(bytes, entry.size - offset);
    const paged = { page, page_count: count, offset, length };
    if (extension === "xml" && entry.size <= bytes) {
      return kind("text", { ...paged, media_type: "application/xml", formatted: true, text: PRETTY_XML });
    }
    if (isJson && entry.size <= bytes) {
      const document = JSON.stringify({ name: key, version: entry.version_id, tags: ["alpha", "beta"], nested: { count: 3, ok: true } });
      return kind("json", {
        ...paged,
        media_type: "application/json",
        formatted: true,
        text: JSON.stringify(JSON.parse(document), null, 2),
      });
    }
    const line = isJson
      ? (n: number) => (n === 0 ? `[${jsonLine(entry.seed, n).slice(1)}` : jsonLine(entry.seed, n))
      : (n: number) => textLine(entry.seed, n);
    return kind(isJson ? "json" : "text", {
      ...paged,
      media_type: isJson ? "application/json" : "text/plain",
      text: byteWindow(line, offset, offset + length),
    });
  }

  /** Demo histories and a few objects of every kind the preview shows. */
  seedDemo(providers: Map<string, Provider>): void {
    const find = (name: string): MockBucket | null => {
      for (const provider of providers.values()) {
        const bucket = provider.buckets.get(name);
        if (bucket !== undefined) {
          return bucket;
        }
      }
      return null;
    };
    const old = (id: string, size: number, at: string, seed: string): MockVersionEntry => ({
      version_id: id,
      is_delete_marker: false,
      size,
      etag: `etag-${id}`,
      last_modified: at,
      storage_class: "STANDARD",
      seed,
    });
    const photos = find("demo-photos");
    if (photos !== null) {
      this.recordOlder(photos.name, "documents/notes.txt", [
        old("v-notes-3", 3_900, "2026-10-14T09:30:00.000Z", "notes-3"),
        old("v-notes-2", 3_512, "2026-10-09T09:30:00.000Z", "notes-2"),
        old("v-notes-1", 2_048, "2026-10-02T09:30:00.000Z", "notes-1"),
      ]);
      this.recordOlder(photos.name, "documents/report-q3.pdf", [old("v-report-1", 1_100_000, "2026-10-05T09:30:00.000Z", "report-1")]);
      this.recordOlder(photos.name, "readme.md", [
        old("v-readme-2", 900, "2026-10-12T09:30:00.000Z", "readme-2"),
        old("v-readme-1", 700, "2026-10-01T09:30:00.000Z", "readme-1"),
      ]);
      // Deleted: only its history is left, with a delete marker on top.
      this.recordOlder(photos.name, "documents/old-draft.txt", [
        old("v-draft-2", 5_120, "2026-10-15T09:30:00.000Z", "draft-2"),
        old("v-draft-1", 2_300, "2026-10-10T09:30:00.000Z", "draft-1"),
      ]);
      this.recordDeleted(photos.name, "documents/old-draft.txt", "2026-10-20T09:30:00.000Z", "dm-draft");
    }
    const website = find("demo-website");
    if (website !== null) {
      this.recordOlder(
        website.name,
        "index.html",
        [4, 3, 2, 1].map((n) => old(`v-index-${n}`, 5_500 + n * 100, `2026-10-0${n}T10:00:00.000Z`, `index-${n}`)),
      );
    }
    // One object of every kind the preview shows, at the top of a bucket that no other
    // demo or test lists at its root.
    const logs = find("demo-logs");
    if (logs !== null) {
      const put = (key: string, size: number, at: string) => logs.objects.set(key, makeObject(key, size, at));
      put("config.json", 380, "2026-10-18T08:00:00.000Z");
      put("sales.csv", 40_000, "2026-10-18T08:02:00.000Z");
      put("events.parquet", 12_000, "2026-10-18T08:03:00.000Z");
      put("catalog.xml", 150, "2026-10-18T08:04:00.000Z");
      put("spinner.gif", 900, "2026-10-18T08:06:00.000Z");
      put("audit-export.log", 600 * 1024, "2026-10-18T08:05:00.000Z");
      put("monitoring-dashboard.webp", 3 * 1024, "2026-10-18T08:10:00.000Z");
      put("site-photo-large.png", 24 * 1024 * 1024, "2026-10-18T08:15:00.000Z");
      put("empty.txt", 0, "2026-10-18T08:20:00.000Z");
      put("snapshot.bin", 2 * 1024 * 1024, "2026-10-18T08:25:00.000Z");
      this.recordOlder(logs.name, "config.json", [old("v-config-1", 320, "2026-10-11T08:00:00.000Z", "config-1")]);
    }
  }
}
