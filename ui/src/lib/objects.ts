import type { ObjectEntry } from "../api/types";
import { parseTimestamp } from "./format";
import { isFolderKey, relativeName } from "./s3keys";

export type SortColumn = "name" | "size" | "modified" | "class";
export type SortDirection = "asc" | "desc";

export interface SortState {
  column: SortColumn;
  direction: SortDirection;
}

export const DEFAULT_SORT: SortState = { column: "name", direction: "asc" };

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** The entry of a folder that is known only by its prefix (a folder of the bucket tree). */
export function folderEntry(prefix: string): ObjectEntry {
  return {
    key: prefix,
    size: 0,
    etag: null,
    last_modified: null,
    storage_class: null,
    checksums: { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null },
    is_prefix: true,
  };
}

/** A prefix entry, or a folder marker object (flat listings return those as keys ending in "/"). */
export function isFolderEntry(entry: ObjectEntry): boolean {
  return entry.is_prefix || isFolderKey(entry.key);
}

function compareByColumn(a: ObjectEntry, b: ObjectEntry, column: SortColumn): number {
  switch (column) {
    case "name":
      return 0;
    case "size":
      return a.size - b.size;
    case "modified":
      return (parseTimestamp(a.last_modified) ?? 0) - (parseTimestamp(b.last_modified) ?? 0);
    case "class":
      return collator.compare(a.storage_class ?? "", b.storage_class ?? "");
  }
}

/**
 * Sorts entries with folders first. Ties on the chosen column fall back to the
 * name, and finally to the raw key, so the order is total and stable.
 */
export function sortEntries(entries: readonly ObjectEntry[], sort: SortState, prefix: string): ObjectEntry[] {
  const factor = sort.direction === "asc" ? 1 : -1;
  return [...entries].sort((a, b) => {
    const folderA = isFolderEntry(a);
    const folderB = isFolderEntry(b);
    if (folderA !== folderB) {
      return folderA ? -1 : 1;
    }
    const byColumn = compareByColumn(a, b, sort.column);
    if (byColumn !== 0) {
      return byColumn * factor;
    }
    const byName = collator.compare(relativeName(a.key, prefix), relativeName(b.key, prefix));
    if (byName !== 0) {
      return sort.column === "name" ? byName * factor : byName;
    }
    if (a.key === b.key) {
      return 0;
    }
    return (a.key < b.key ? -1 : 1) * (sort.column === "name" ? factor : 1);
  });
}

/** Case-insensitive search over the names of loaded entries. */
export function filterEntries(entries: readonly ObjectEntry[], query: string, prefix: string): ObjectEntry[] {
  const needle = query.trim().toLocaleLowerCase();
  if (needle.length === 0) {
    return [...entries];
  }
  return entries.filter((entry) => relativeName(entry.key, prefix).toLocaleLowerCase().includes(needle));
}

export function nextSort(current: SortState, column: SortColumn): SortState {
  if (current.column === column) {
    return { column, direction: current.direction === "asc" ? "desc" : "asc" };
  }
  return { column, direction: "asc" };
}
