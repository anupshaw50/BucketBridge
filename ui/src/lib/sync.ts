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
