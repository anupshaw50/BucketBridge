import type { IfExists } from "../api/types";
import { parentPrefix, pasteProblem } from "./s3keys";

/** The objects being dragged: rows of one bucket of one account. */
export interface DragSource {
  accountId: string;
  bucket: string;
  keys: string[];
}

/** A folder of a bucket that the pointer is over. */
export interface DropTarget {
  accountId: string;
  bucket: string;
  prefix: string;
}

export interface TransferArgs {
  account_id: string;
  src_bucket: string;
  keys: string[];
  dst_bucket: string;
  dst_prefix: string;
  dst_account_id?: string | null;
  if_exists?: IfExists | null;
}

/** What a drop does. "noop" is silent (the items are already there); "blocked" explains itself. */
export type DropPlan =
  | { kind: "noop" }
  | { kind: "blocked"; message: string }
  | { kind: "transfer"; mode: "copy" | "move"; args: TransferArgs };

/**
 * Decides what dropping `source` on `target` does. A drop copies unless `move`
 * is set. Dropping into the folder the items already live in does nothing, a
 * folder cannot go into itself, and the account of the destination is passed
 * as `dst_account_id` when it differs from the account of the items.
 */
export function planDrop(source: DragSource, target: DropTarget, move: boolean): DropPlan {
  const sameAccount = source.accountId === target.accountId;
  if (source.keys.length === 0) {
    return { kind: "noop" };
  }
  if (sameAccount && source.bucket === target.bucket && source.keys.every((key) => parentPrefix(key) === target.prefix)) {
    return { kind: "noop" };
  }
  // Buckets of different accounts are different buckets even when the names match.
  const sourceBucket = sameAccount ? source.bucket : `${source.accountId}\u0000${source.bucket}`;
  const problem = pasteProblem(sourceBucket, source.keys, target.bucket, target.prefix);
  if (problem !== null) {
    return { kind: "blocked", message: problem };
  }
  const args: TransferArgs = {
    account_id: source.accountId,
    src_bucket: source.bucket,
    keys: source.keys,
    dst_bucket: target.bucket,
    dst_prefix: target.prefix,
  };
  if (!sameAccount) {
    args.dst_account_id = target.accountId;
  }
  return { kind: "transfer", mode: move ? "move" : "copy", args };
}

/** The key that turns a drop into a move: Command on macOS, Shift elsewhere. */
export function moveModifier(userAgent: string = navigator.userAgent): "meta" | "shift" {
  return userAgent.includes("Mac") ? "meta" : "shift";
}

/**
 * The CSS-pixel position of a file drop reported by the host. Tauri labels the
 * position "physical", but only WebView2 on Windows reports device pixels; on
 * macOS (WKWebView) and Linux (WebKitGTK) it is already in logical points, so
 * dividing there would put the drop at half the distance from the top left
 * corner, on a different target.
 */
export function dropPositionToCss(
  position: { x: number; y: number },
  scale: number,
  userAgent: string = navigator.userAgent,
): { x: number; y: number } {
  const factor = userAgent.includes("Windows") && scale > 0 ? scale : 1;
  return { x: position.x / factor, y: position.y / factor };
}

export function isMoveDrag(event: { metaKey: boolean; shiftKey: boolean }, userAgent?: string): boolean {
  return moveModifier(userAgent) === "meta" ? event.metaKey : event.shiftKey;
}
