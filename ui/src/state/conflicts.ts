import { createStore, type StoreApi } from "zustand/vanilla";
import type { ConflictEvent, PendingConflict } from "../api/types";

export interface ConflictState {
  /** Tasks that wait for an overwrite decision, oldest first. */
  pending: readonly PendingConflict[];
}

/**
 * The overwrite questions of tasks that run with the policy "ask". They arrive
 * as `transfer://conflict` events; the snapshot from `list_conflicts` fills in
 * what was asked before the window started listening.
 */
export class ConflictStore {
  readonly store: StoreApi<ConflictState> = createStore<ConflictState>(() => ({ pending: [] }));

  onEvent(event: ConflictEvent): void {
    if (event.type === "resolved") {
      this.remove(event.task_id);
      return;
    }
    const pending: PendingConflict = { task_id: event.task_id, group_id: event.group_id, conflict: event.conflict };
    const others = this.store.getState().pending.filter((item) => item.task_id !== pending.task_id);
    this.store.setState({ pending: [...others, pending] });
  }

  /** The `list_conflicts` snapshot; questions that arrived by event meanwhile are kept. */
  mergeSnapshot(snapshot: readonly PendingConflict[]): void {
    const known = new Set(this.store.getState().pending.map((item) => item.task_id));
    const added = snapshot.filter((item) => !known.has(item.task_id));
    if (added.length > 0) {
      this.store.setState({ pending: [...this.store.getState().pending, ...added] });
    }
  }

  remove(taskId: string): void {
    const pending = this.store.getState().pending;
    if (pending.some((item) => item.task_id === taskId)) {
      this.store.setState({ pending: pending.filter((item) => item.task_id !== taskId) });
    }
  }
}
