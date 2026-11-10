import type { Location, TransferKind, TransferState, TransferTask } from "../api/types";
import { tr } from "../i18n";
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
  queued: tr("Queued"),
  running: tr("Running"),
  stopped: tr("Stopped"),
  completed: tr("Completed"),
  failed: tr("Failed"),
  cancelled: tr("Cancelled"),
  skipped: tr("Skipped"),
};

export const KIND_LABELS: Record<TransferKind, string> = {
  upload: tr("Upload"),
  download: tr("Download"),
  copy: tr("Copy"),
  move: tr("Move"),
  delete: tr("Delete"),
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
      return tr("{n} objects", { n: formatCount(task.bytes_done) });
    }
    return tr("{done} of {total} objects", { done: formatCount(task.bytes_done), total: formatCount(task.bytes_total) });
  }
  if (task.bytes_total === null) {
    return formatBytes(task.bytes_done);
  }
  return tr("{done} of {total}", { done: formatBytes(task.bytes_done), total: formatBytes(task.bytes_total) });
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

/**
 * Percent done over the bytes of the transfers that are queued or running, or
 * null when nothing is under way (then no overall bar is shown).
 */
export function overallProgress(tasks: Iterable<TransferTask>): number | null {
  let done = 0;
  let total = 0;
  let active = false;
  for (const task of tasks) {
    if (task.state !== "queued" && task.state !== "running") continue;
    active = true;
    if (task.bytes_total !== null && task.bytes_total > 0) {
      total += task.bytes_total;
      done += Math.min(task.bytes_done, task.bytes_total);
    }
  }
  if (!active) return null;
  return total > 0 ? (done / total) * 100 : 0;
}
