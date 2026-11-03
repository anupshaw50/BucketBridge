import type { Side, SyncAction, SyncDirection, SyncRow } from "../api/types";

export type SyncRowFilter = "all" | "failed";

export type SyncCounts = Record<SyncAction, number>;

export function syncCounts(rows: readonly SyncRow[]): SyncCounts {
  const counts: SyncCounts = { copy: 0, delete: 0, equal: 0, failed: 0 };
  for (const row of rows) {
    counts[row.action] += 1;
  }
  return counts;
}

/** The "failed only" view: rows whose action is failed or whose error is set. */
export function isFailedRow(row: SyncRow): boolean {
  return row.action === "failed" || row.error !== null;
}

/** Copy and delete rows run by sync_execute; analysis-time failed rows cannot be executed. */
export function isExecutable(row: SyncRow): boolean {
  return row.action === "copy" || row.action === "delete";
}

/** Indexes (into `rows`) visible under the given row switcher. */
export function visibleRowIndexes(rows: readonly SyncRow[], filter: SyncRowFilter): number[] {
  const indexes: number[] = [];
  rows.forEach((row, index) => {
    if (filter === "all" || isFailedRow(row)) {
      indexes.push(index);
    }
  });
  return indexes;
}

/** Rows a call to sync_execute would still process: copy and delete rows that are not done. */
export function pendingRows(rows: readonly SyncRow[]): number {
  return rows.filter((row) => isExecutable(row) && !row.done).length;
}

export const DIRECTION_LABELS: Record<SyncDirection, string> = {
  upload: "Upload (local to S3)",
  download: "Download (S3 to local)",
  bidirectional: "Both directions",
  s3_to_s3: "S3 to S3 (between prefixes)",
};

export const ACTION_LABELS: Record<SyncAction, string> = {
  copy: "Copy",
  delete: "Delete",
  equal: "Equal",
  failed: "Failed",
};

export const SIDE_LABELS: Record<Side, string> = {
  local: "Local",
  remote: "S3",
};

/** Splits "s3://bucket/prefix/" into its parts, or returns null when malformed. */
export function parseS3Prefix(uri: string): { bucket: string; prefix: string } | null {
  const match = /^s3:\/\/([^/]+)\/(.*)$/.exec(uri);
  if (!match) {
    return null;
  }
  const [, bucket = "", prefix = ""] = match;
  if (bucket.length === 0 || (prefix.length > 0 && !prefix.endsWith("/"))) {
    return null;
  }
  return { bucket, prefix };
}

/** `850 ms`, `12.3 s`, `4 min 5 s` or `2 h 3 min`. */
export function formatRunDuration(millis: number): string {
  if (millis < 1000) {
    return `${millis} ms`;
  }
  const seconds = Math.floor(millis / 1000);
  if (seconds < 60) {
    return `${(millis / 1000).toFixed(1)} s`;
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes} min ${seconds % 60} s`;
  }
  return `${Math.floor(minutes / 60)} h ${minutes % 60} min`;
}
