import type { Location, TransferKind, TransferState, TransferTask } from "../api/types";
import { formatBytes, formatCount } from "./format";

export type TransferView = "all" | "queued" | "running" | "stopped" | "failed";

export const TRANSFER_VIEWS: readonly TransferView[] = ["all", "queued", "running", "stopped", "failed"];

export type TransferCounts = Record<TransferState | "all", number>;

export function emptyCounts(): TransferCounts {
  return { all: 0, queued: 0, running: 0, stopped: 0, completed: 0, failed: 0, cancelled: 0, skipped: 0 };
}

export function inView(view: TransferView, state: TransferState): boolean {
  return view === "all" || view === state;
}

// Queue rules from the contract: pause and stop move queued or running tasks to
// stopped, resume and start move stopped tasks to queued, retry re-queues a
// failed task, and cancel aborts any task that has not finished.
export function canPause(state: TransferState): boolean {
  return state === "queued" || state === "running";
}

export function canResume(state: TransferState): boolean {
  return state === "stopped";
}

export function canRetry(state: TransferState): boolean {
  return state === "failed";
}

export function canCancel(state: TransferState): boolean {
  return state === "queued" || state === "running" || state === "stopped" || state === "failed";
}

export function isFinished(state: TransferState): boolean {
  return state === "completed" || state === "cancelled" || state === "skipped";
}

export const STATE_LABELS: Record<TransferState, string> = {
  queued: "Queued",
  running: "Running",
  stopped: "Stopped",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
  skipped: "Skipped",
};

export const KIND_LABELS: Record<TransferKind, string> = {
  upload: "Upload",
  download: "Download",
  copy: "Copy",
  move: "Move",
  delete: "Delete",
};

export function describeLocation(location: Location | null): string {
  if (location === null) {
    return "";
  }
  if (location.type === "local") {
    return location.path;
  }
  return `s3://${location.bucket}/${location.key}`;
}

/** Progress text: bytes for transfers, object counts for deletes. */
export function progressText(task: TransferTask): string {
  if (task.kind === "delete") {
    if (task.bytes_total === null) {
      return `${formatCount(task.bytes_done)} objects`;
    }
    return `${formatCount(task.bytes_done)} of ${formatCount(task.bytes_total)} objects`;
  }
  if (task.bytes_total === null) {
    return formatBytes(task.bytes_done);
  }
  return `${formatBytes(task.bytes_done)} of ${formatBytes(task.bytes_total)}`;
}

/** Does this task change the listing of `bucket`/`prefix` once it finishes? */
export function touchesListing(task: TransferTask, accountId: string, bucket: string, prefix: string): boolean {
  if (task.account_id !== accountId) {
    return false;
  }
  const touches = (location: Location | null) =>
    location !== null && location.type === "s3" && location.bucket === bucket && location.key.startsWith(prefix);
  if (task.kind === "download") {
    return false;
  }
  if (task.kind === "move" || task.kind === "delete") {
    return touches(task.src) || touches(task.dst);
  }
  return touches(task.dst);
}
