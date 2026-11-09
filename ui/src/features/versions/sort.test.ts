import { describe, expect, it } from "vitest";
import type { ObjectVersion, VersionGroup } from "../../api/types";
import { versionId } from "./filters";
import {
  flatVersions,
  newestDeleteMarkerIds,
  nextVersionSort,
  nonCurrentIds,
  sortGroups,
  sortVersions,
} from "./sort";

function version(key: string, id: string, opts: Partial<ObjectVersion> = {}): ObjectVersion {
  return {
    key,
    version_id: id,
    is_latest: false,
    is_delete_marker: false,
    size: 10,
    etag: null,
    last_modified: "2026-01-01T00:00:00Z",
    storage_class: null,
    ...opts,
  };
}

function group(key: string, versions: ObjectVersion[]): VersionGroup {
  return {
    key,
    versions,
    total: versions.length,
    matching: versions.length,
    is_deleted: versions[0]?.is_delete_marker === true,
    truncated: false,
  };
}

const GROUPS = [
  group("file10.txt", [
    version("file10.txt", "a3", { is_latest: true, size: 30, last_modified: "2026-03-01T00:00:00Z" }),
    version("file10.txt", "a1", { size: 5, last_modified: "2026-01-01T00:00:00Z" }),
  ]),
  group("file2.txt", [
    version("file2.txt", "b3", { is_latest: true, is_delete_marker: true, size: 0, last_modified: "2026-04-01T00:00:00Z" }),
    version("file2.txt", "b2", { size: 20, last_modified: "2026-02-01T00:00:00Z" }),
    version("file2.txt", "b1", { is_delete_marker: true, size: 0, last_modified: "2026-01-15T00:00:00Z" }),
  ]),
];

describe("version sorting", () => {
  it("cycles ascending, descending, and back to the server order", () => {
    const first = nextVersionSort(null, "size");
    expect(first).toEqual({ column: "size", direction: "asc" });
    const second = nextVersionSort(first, "size");
    expect(second).toEqual({ column: "size", direction: "desc" });
    expect(nextVersionSort(second, "size")).toBeNull();
    expect(nextVersionSort(second, "modified")).toEqual({ column: "modified", direction: "asc" });
  });

  it("sorts a flat list by every column", () => {
    const ids = (sort: Parameters<typeof flatVersions>[1]) => flatVersions(GROUPS, sort).map((v) => v.version_id);
    expect(ids(null)).toEqual(["a3", "a1", "b3", "b2", "b1"]);
    expect(ids({ column: "size", direction: "desc" })).toEqual(["a3", "b2", "a1", "b3", "b1"]);
    expect(ids({ column: "modified", direction: "asc" })).toEqual(["a1", "b1", "b2", "a3", "b3"]);
    expect(ids({ column: "version", direction: "desc" })).toEqual(["b3", "b2", "b1", "a3", "a1"]);
    // Natural order of keys: file2 before file10.
    expect(ids({ column: "key", direction: "asc" })).toEqual(["b3", "b2", "b1", "a3", "a1"]);
    expect(ids({ column: "key", direction: "desc" })).toEqual(["a3", "a1", "b3", "b2", "b1"]);
  });

  it("keeps the input order for equal values", () => {
    const equal = [version("k", "x"), version("k", "y"), version("k", "z")];
    expect(sortVersions(equal, { column: "size", direction: "desc" }).map((v) => v.version_id)).toEqual(["x", "y", "z"]);
  });

  it("sorts inside the groups, and orders the groups by key only for the key column", () => {
    const bySize = sortGroups(GROUPS, { column: "size", direction: "asc" });
    expect(bySize.map((g) => g.key)).toEqual(["file10.txt", "file2.txt"]);
    expect(bySize[1]?.versions.map((v) => v.version_id)).toEqual(["b3", "b1", "b2"]);
    const byKey = sortGroups(GROUPS, { column: "key", direction: "asc" });
    expect(byKey.map((g) => g.key)).toEqual(["file2.txt", "file10.txt"]);
  });
});

describe("version selection helpers", () => {
  it("finds the delete markers that are the newest entry of a key", () => {
    expect(newestDeleteMarkerIds(GROUPS)).toEqual([versionId("file2.txt", "b3")]);
  });

  it("finds every entry that is not the current version, delete markers included", () => {
    expect(nonCurrentIds(GROUPS)).toEqual([
      versionId("file10.txt", "a1"),
      versionId("file2.txt", "b2"),
      versionId("file2.txt", "b1"),
    ]);
  });
});
