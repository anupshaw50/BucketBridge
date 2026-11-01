import { createStore, type StoreApi } from "zustand/vanilla";
import type { TransferTask } from "../api/types";
import { parseTimestamp } from "../lib/format";
import { emptyCounts, type TransferCounts } from "../lib/transfers";
import { type FrameScheduler, FrameTask } from "./scheduler";

export interface TransferStoreState {
  tasks: ReadonlyMap<string, TransferTask>;
  /** Task ids in display order (oldest first). */
  order: readonly string[];
  counts: TransferCounts;
  /** Increments on every flush; cheap dependency for memoised views. */
  version: number;
}

function countTasks(tasks: ReadonlyMap<string, TransferTask>): TransferCounts {
  const counts = emptyCounts();
  for (const task of tasks.values()) {
    counts.all += 1;
    counts[task.state] += 1;
  }
  return counts;
}

function isNewer(candidate: TransferTask, existing: TransferTask | undefined): boolean {
  if (existing === undefined) {
    return true;
  }
  const a = parseTimestamp(candidate.updated_at);
  const b = parseTimestamp(existing.updated_at);
  if (a === null || b === null) {
    return true;
  }
  return a >= b;
}

/**
 * Holds every transfer task. Updates arrive as events (possibly thousands per
 * second when a large folder is queued) and are coalesced per animation
 * frame: only the latest version of each task is applied, once per frame.
 */
export class TransferStore {
  readonly store: StoreApi<TransferStoreState>;
  private readonly pending = new Map<string, TransferTask | null>();
  private readonly frame: FrameTask;
  private readonly listeners = new Set<(changed: readonly TransferTask[]) => void>();

  constructor(scheduler: FrameScheduler) {
    this.frame = new FrameTask(scheduler, () => this.apply());
    this.store = createStore<TransferStoreState>(() => ({
      tasks: new Map(),
      order: [],
      counts: emptyCounts(),
      version: 0,
    }));
  }

  /** Called after each flush with the tasks that changed (not the removed ones). */
  onFlush(listener: (changed: readonly TransferTask[]) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** A `transfer://update` event: always the newest state of that task. */
  upsert(task: TransferTask): void {
    this.pending.set(task.id, task);
    this.schedule();
  }

  /** A `transfer://removed` event. */
  remove(ids: readonly string[]): void {
    for (const id of ids) {
      this.pending.set(id, null);
    }
    this.schedule();
  }

  /**
   * Tasks returned by an enqueue command. Events for the same tasks may have
   * arrived first, so a task is only added when nothing newer is known.
   */
  addReturned(tasks: readonly TransferTask[]): void {
    const current = this.store.getState().tasks;
    for (const task of tasks) {
      const known = this.pending.has(task.id) ? this.pending.get(task.id) : current.get(task.id);
      if (known === undefined) {
        this.pending.set(task.id, task);
      }
    }
    this.schedule();
  }

  /** The `list_tasks` snapshot; anything newer that arrived by event is kept. */
  mergeSnapshot(tasks: readonly TransferTask[]): void {
    const current = this.store.getState().tasks;
    for (const task of tasks) {
      const pendingTask = this.pending.get(task.id);
      if (pendingTask === null) {
        continue; // removed while the snapshot was in flight
      }
      const known = pendingTask ?? current.get(task.id);
      if (isNewer(task, known)) {
        this.pending.set(task.id, task);
      }
    }
    this.schedule();
  }

  get(id: string): TransferTask | undefined {
    const pendingTask = this.pending.get(id);
    if (pendingTask === null) {
      return undefined;
    }
    return pendingTask ?? this.store.getState().tasks.get(id);
  }

  /** Applies pending updates now (used by tests and before reading derived state). */
  flush(): void {
    this.frame.cancel();
    this.apply();
  }

  private schedule(): void {
    if (this.pending.size > 0) {
      this.frame.request();
    }
  }

  private apply(): void {
    if (this.pending.size === 0) {
      return;
    }
    const state = this.store.getState();
    const tasks = new Map(state.tasks);
    const appended: string[] = [];
    const changed: TransferTask[] = [];
    let removedAny = false;
    for (const [id, task] of this.pending) {
      if (task === null) {
        removedAny = tasks.delete(id) || removedAny;
      } else {
        if (!tasks.has(id)) {
          appended.push(id);
        }
        tasks.set(id, task);
        changed.push(task);
      }
    }
    this.pending.clear();
    let order = state.order;
    if (removedAny) {
      order = order.filter((id) => tasks.has(id));
    }
    if (appended.length > 0) {
      const present = new Set(order);
      order = [...order, ...appended.filter((id) => !present.has(id))];
    }
    this.store.setState({ tasks, order, counts: countTasks(tasks), version: state.version + 1 });
    if (changed.length > 0) {
      for (const listener of this.listeners) {
        listener(changed);
      }
    }
  }
}
