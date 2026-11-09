import type { Bucket } from "../api/types";
import { parseTimestamp } from "./format";
import { naturalCompare } from "./objects";

/** Narrowing of the bucket list. Empty values match everything. */
export interface BucketFilter {
  /** Case-insensitive part of the bucket name. */
  name: string;
  /** Region code; empty = any. */
  region: string;
  /** First day (local, "YYYY-MM-DD") the bucket may have been created on; empty = no limit. */
  createdFrom: string;
  /** Last day (local, "YYYY-MM-DD") the bucket may have been created on; empty = no limit. */
  createdTo: string;
}

export const EMPTY_BUCKET_FILTER: BucketFilter = { name: "", region: "", createdFrom: "", createdTo: "" };

export function hasBucketFilter(filter: BucketFilter): boolean {
  return filter.name.trim() !== "" || filter.region !== "" || filter.createdFrom !== "" || filter.createdTo !== "";
}

/** Sorts buckets by name in natural order (`bucket2` before `bucket10`). */
export function sortBuckets(buckets: readonly Bucket[]): Bucket[] {
  return [...buckets].sort((a, b) => naturalCompare(a.name, b.name) || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}

function startOfDay(day: string): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (match === null) {
    return null;
  }
  return new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3])).getTime();
}

/** Buckets that match every part of the filter. A creation date filter excludes buckets without a date. */
export function filterBuckets(buckets: readonly Bucket[], filter: BucketFilter): Bucket[] {
  const needle = filter.name.trim().toLocaleLowerCase();
  const from = startOfDay(filter.createdFrom);
  const toStart = startOfDay(filter.createdTo);
  // The last day is included: the limit is the start of the next day.
  const to = toStart === null ? null : toStart + 24 * 60 * 60 * 1000;
  return buckets.filter((bucket) => {
    if (needle !== "" && !bucket.name.toLocaleLowerCase().includes(needle)) {
      return false;
    }
    if (filter.region !== "" && bucket.region !== filter.region) {
      return false;
    }
    if (from !== null || to !== null) {
      const created = parseTimestamp(bucket.creation_date);
      if (created === null) {
        return false;
      }
      if (from !== null && created < from) {
        return false;
      }
      if (to !== null && created >= to) {
        return false;
      }
    }
    return true;
  });
}

/** Distinct regions of the buckets, sorted. */
export function bucketRegions(buckets: readonly Bucket[]): string[] {
  const regions = new Set<string>();
  for (const bucket of buckets) {
    if (bucket.region !== null && bucket.region !== "") {
      regions.add(bucket.region);
    }
  }
  return [...regions].sort();
}
