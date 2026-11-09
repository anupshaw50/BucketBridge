import type { ObjectVersion, VersionGroup } from "../../api/types";
import { parseTimestamp } from "../../lib/format";
import { naturalCompare } from "../../lib/objects";
import { versionId } from "./filters";

export type VersionSortColumn = "key" | "version" | "modified" | "size";
export interface VersionSort {
  column: VersionSortColumn;
  direction: "asc" | "desc";
}

/** Clicking a header: ascending, then descending, then the order the server sent. */
export function nextVersionSort(current: VersionSort | null, column: VersionSortColumn): VersionSort | null {
  if (current?.column !== column) {
    return { column, direction: "asc" };
  }
  return current.direction === "asc" ? { column, direction: "desc" } : null;
}

function compareText(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function compare(a: ObjectVersion, b: ObjectVersion, column: VersionSortColumn): number {
  switch (column) {
    case "key":
      return naturalCompare(a.key, b.key) || compareText(a.key, b.key);
    case "version":
      return compareText(a.version_id, b.version_id);
    case "modified":
      return (parseTimestamp(a.last_modified) ?? 0) - (parseTimestamp(b.last_modified) ?? 0);
    case "size":
      return a.size - b.size;
  }
}

/** The versions in the chosen order; without a sort, the order they came in (newest first within a key). */
export function sortVersions(versions: readonly ObjectVersion[], sort: VersionSort | null): ObjectVersion[] {
  if (sort === null) {
    return [...versions];
  }
  const factor = sort.direction === "asc" ? 1 : -1;
  const indexed = versions.map((version, index) => ({ version, index }));
  indexed.sort((x, y) => compare(x.version, y.version, sort.column) * factor || x.index - y.index);
  return indexed.map((entry) => entry.version);
}

/** Groups with their versions sorted; sorting by key also orders the groups. */
export function sortGroups(groups: readonly VersionGroup[], sort: VersionSort | null): VersionGroup[] {
  if (sort === null) {
    return [...groups];
  }
  const sorted = groups.map((group) => ({ ...group, versions: sortVersions(group.versions, sort) }));
  if (sort.column === "key") {
    const factor = sort.direction === "asc" ? 1 : -1;
    sorted.sort((a, b) => (naturalCompare(a.key, b.key) || compareText(a.key, b.key)) * factor);
  }
  return sorted;
}

/** Every loaded entry in one list, as the flat view shows them. */
export function flatVersions(groups: readonly VersionGroup[], sort: VersionSort | null): ObjectVersion[] {
  return sortVersions(
    groups.flatMap((group) => group.versions),
    sort,
  );
}

/** Ids of the delete markers that are the newest entry of their key (the keys that look deleted). */
export function newestDeleteMarkerIds(groups: readonly VersionGroup[]): string[] {
  return groups.flatMap((group) =>
    group.versions.filter((v) => v.is_delete_marker && v.is_latest).map((v) => versionId(v.key, v.version_id)),
  );
}

/** Ids of every entry that is not the current version of its key. */
export function nonCurrentIds(groups: readonly VersionGroup[]): string[] {
  return groups.flatMap((group) => group.versions.filter((v) => !v.is_latest).map((v) => versionId(v.key, v.version_id)));
}
