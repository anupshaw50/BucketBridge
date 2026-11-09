import { describe, expect, it } from "vitest";
import type { Bucket } from "../api/types";
import { bucketRegions, EMPTY_BUCKET_FILTER, filterBuckets, hasBucketFilter, sortBuckets } from "./buckets";

function bucket(name: string, region: string | null, created: string | null): Bucket {
  return { name, region, creation_date: created, owner: null, kind: "aws" };
}

const BUCKETS = [
  bucket("logs-10", "us-east-1", "2026-03-10T12:00:00Z"),
  bucket("logs-2", "eu-west-1", "2026-01-05T08:00:00Z"),
  bucket("Photos", "us-east-1", "2025-12-31T23:30:00Z"),
  bucket("misc", null, null),
];

describe("bucket list", () => {
  it("sorts names naturally", () => {
    expect(sortBuckets(BUCKETS).map((item) => item.name)).toEqual(["logs-2", "logs-10", "misc", "Photos"]);
    expect(sortBuckets([bucket("b10", null, null), bucket("b9", null, null), bucket("b1", null, null)]).map((item) => item.name)).toEqual([
      "b1",
      "b9",
      "b10",
    ]);
  });

  it("filters by name, ignoring case", () => {
    expect(filterBuckets(BUCKETS, { ...EMPTY_BUCKET_FILTER, name: "LOG" }).map((item) => item.name)).toEqual(["logs-10", "logs-2"]);
    expect(filterBuckets(BUCKETS, EMPTY_BUCKET_FILTER)).toHaveLength(4);
  });

  it("filters by region", () => {
    expect(filterBuckets(BUCKETS, { ...EMPTY_BUCKET_FILTER, region: "eu-west-1" }).map((item) => item.name)).toEqual(["logs-2"]);
    expect(bucketRegions(BUCKETS)).toEqual(["eu-west-1", "us-east-1"]);
  });

  it("filters by creation date, inclusive of both days, and drops buckets without a date", () => {
    const after = filterBuckets(BUCKETS, { ...EMPTY_BUCKET_FILTER, createdFrom: "2026-01-01" });
    expect(after.map((item) => item.name)).toEqual(["logs-10", "logs-2"]);
    const before = filterBuckets(BUCKETS, { ...EMPTY_BUCKET_FILTER, createdTo: "2026-01-05" });
    expect(before.map((item) => item.name)).toEqual(["logs-2", "Photos"]);
    const between = filterBuckets(BUCKETS, { ...EMPTY_BUCKET_FILTER, createdFrom: "2026-01-05", createdTo: "2026-01-05" });
    expect(between.map((item) => item.name)).toEqual(["logs-2"]);
  });

  it("combines the parts and tells whether a filter is set", () => {
    const combined = filterBuckets(BUCKETS, { name: "log", region: "us-east-1", createdFrom: "", createdTo: "" });
    expect(combined.map((item) => item.name)).toEqual(["logs-10"]);
    expect(hasBucketFilter(EMPTY_BUCKET_FILTER)).toBe(false);
    expect(hasBucketFilter({ ...EMPTY_BUCKET_FILTER, name: "  " })).toBe(false);
    expect(hasBucketFilter({ ...EMPTY_BUCKET_FILTER, region: "x" })).toBe(true);
  });
});
