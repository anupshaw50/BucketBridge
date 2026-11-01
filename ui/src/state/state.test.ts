import { describe, expect, it, vi } from "vitest";
import type { Backend } from "../api/backend";
import type { LogEntry, TransferTask } from "../api/types";
import { LogController } from "./log";
import { PromptController } from "./prompts";
import { FrameTask, immediateScheduler, type FrameScheduler } from "./scheduler";
import { TransferStore } from "./transferStore";

/** A scheduler that runs queued callbacks only when the test says so. */
function manualScheduler(): FrameScheduler & { run: () => void; pending: () => number } {
  let queue: (() => void)[] = [];
  const scheduler = ((callback: () => void) => {
    queue.push(callback);
    return () => {
      queue = queue.filter((item) => item !== callback);
    };
  }) as FrameScheduler & { run: () => void; pending: () => number };
  scheduler.run = () => {
    const current = queue;
    queue = [];
    for (const callback of current) {
      callback();
    }
  };
  scheduler.pending = () => queue.length;
  return scheduler;
}

function task(id: string, overrides: Partial<TransferTask> = {}): TransferTask {
  return {
    id,
    account_id: "a",
    group_id: null,
    kind: "upload",
    src: { type: "local", path: `/tmp/${id}` },
    dst: { type: "s3", bucket: "b", key: id },
    bytes_total: 100,
    bytes_done: 0,
    speed: 0,
    eta: null,
    state: "queued",
    error: null,
    attempts: 0,
    created_at: "2026-10-29T08:00:00Z",
    updated_at: "2026-10-29T08:00:00Z",
    ...overrides,
  };
}

describe("FrameTask", () => {
  it("runs once per frame and keeps working with a synchronous scheduler", () => {
    const run = vi.fn();
    const sync = new FrameTask(immediateScheduler, run);
    sync.request();
    sync.request();
    expect(run).toHaveBeenCalledTimes(2);
    const scheduler = manualScheduler();
    const batched = new FrameTask(scheduler, run);
    batched.request();
    batched.request();
    expect(scheduler.pending()).toBe(1);
    scheduler.run();
    expect(run).toHaveBeenCalledTimes(3);
    batched.request();
    batched.cancel();
    scheduler.run();
    expect(run).toHaveBeenCalledTimes(3);
    batched.request();
    scheduler.run();
    expect(run).toHaveBeenCalledTimes(4);
  });
});

describe("TransferStore", () => {
  it("coalesces a burst of 10,000 tasks into one store update per frame", () => {
    const scheduler = manualScheduler();
    const store = new TransferStore(scheduler);
    const listener = vi.fn();
    store.store.subscribe(listener);
    for (let index = 0; index < 10_000; index += 1) {
      store.upsert(task(`t${index}`));
    }
    for (let index = 0; index < 10_000; index += 1) {
      store.upsert(task(`t${index}`, { state: "running", bytes_done: 50, updated_at: "2026-10-29T08:00:01Z" }));
    }
    expect(listener).not.toHaveBeenCalled();
    expect(scheduler.pending()).toBe(1);
    scheduler.run();
    expect(listener).toHaveBeenCalledTimes(1);
    const state = store.store.getState();
    expect(state.order).toHaveLength(10_000);
    expect(state.order[0]).toBe("t0");
    expect(state.counts).toMatchObject({ all: 10_000, running: 10_000, queued: 0 });
    expect(state.tasks.get("t9999")?.bytes_done).toBe(50);
  });

  it("applies removals in event order and keeps the display order", () => {
    const store = new TransferStore(immediateScheduler);
    store.upsert(task("a"));
    store.upsert(task("b"));
    store.upsert(task("c"));
    store.remove(["b"]);
    expect(store.store.getState().order).toEqual(["a", "c"]);
    store.upsert(task("b", { state: "completed" }));
    expect(store.store.getState().order).toEqual(["a", "c", "b"]);
    expect(store.store.getState().counts.completed).toBe(1);
  });

  it("does not let an older enqueue result or snapshot overwrite newer events", () => {
    const scheduler = manualScheduler();
    const store = new TransferStore(scheduler);
    store.upsert(task("x", { state: "running", bytes_done: 70, updated_at: "2026-10-29T08:00:05Z" }));
    store.addReturned([task("x")]);
    store.mergeSnapshot([task("x", { updated_at: "2026-10-29T08:00:01Z" }), task("y")]);
    scheduler.run();
    expect(store.store.getState().tasks.get("x")).toMatchObject({ state: "running", bytes_done: 70 });
    expect(store.store.getState().tasks.has("y")).toBe(true);
    store.remove(["y"]);
    store.mergeSnapshot([task("y")]);
    scheduler.run();
    expect(store.store.getState().tasks.has("y")).toBe(false);
  });

  it("notifies flush listeners with the changed tasks", () => {
    const store = new TransferStore(immediateScheduler);
    const changed: string[][] = [];
    const stop = store.onFlush((tasks) => changed.push(tasks.map((item) => item.id)));
    store.upsert(task("a"));
    stop();
    store.upsert(task("b"));
    expect(changed).toEqual([["a"]]);
  });
});

describe("PromptController", () => {
  function backend(unlocked: boolean): Backend {
    return {
      security_status: () => Promise.resolve({ master_password_set: true, unlocked }),
    } as unknown as Backend;
  }

  it("shares one unlock dialog between concurrent requests", async () => {
    const prompts = new PromptController(backend(false));
    const first = prompts.requestUnlock();
    const second = prompts.requestUnlock();
    await vi.waitFor(() => expect(prompts.store.getState().unlockOpen).toBe(true));
    prompts.finishUnlock(true);
    await expect(Promise.all([first, second])).resolves.toEqual([true, true]);
    expect(prompts.store.getState().unlockOpen).toBe(false);
  });

  it("does not prompt when the secrets are already unlocked", async () => {
    const prompts = new PromptController(backend(true));
    await expect(prompts.requestUnlock()).resolves.toBe(true);
    expect(prompts.store.getState().unlockOpen).toBe(false);
  });

  it("queues MFA prompts per account and reuses a fresh login", async () => {
    const prompts = new PromptController(backend(true));
    const a1 = prompts.requestMfa("a");
    const b1 = prompts.requestMfa("b");
    const a2 = prompts.requestMfa("a");
    expect(prompts.store.getState().mfaAccountId).toBe("a");
    prompts.finishMfa(true);
    await expect(Promise.all([a1, a2])).resolves.toEqual([true, true]);
    expect(prompts.store.getState().mfaAccountId).toBe("b");
    prompts.finishMfa(false);
    await expect(b1).resolves.toBe(false);
    await expect(prompts.requestMfa("a")).resolves.toBe(true);
    expect(prompts.store.getState().mfaAccountId).toBeNull();
  });
});

function entry(id: number, level: LogEntry["level"], message: string): LogEntry {
  return { id, ts: "2026-10-29T08:00:00Z", level, source: "test", message };
}

describe("LogController", () => {
  it("keeps live entries that arrive while get_log is in flight, without duplicates", async () => {
    let release: (entries: LogEntry[]) => void = () => undefined;
    const api = {
      get_log: vi.fn(() => new Promise<LogEntry[]>((resolve) => (release = resolve))),
    } as unknown as Backend;
    const log = new LogController(api, immediateScheduler);
    const loading = log.load();
    log.append(entry(2, "error", "during load"));
    log.append(entry(3, "information", "also during load"));
    release([entry(1, "information", "old"), entry(2, "error", "during load")]);
    await loading;
    expect(log.store.getState().entries.map((item) => item.id)).toEqual([1, 2, 3]);
    log.append(entry(3, "information", "duplicate"));
    log.append(entry(4, "warning", "new"));
    expect(log.store.getState().entries.map((item) => item.id)).toEqual([1, 2, 3, 4]);
  });

  it("only appends live entries that match the filter", async () => {
    const api = { get_log: vi.fn(() => Promise.resolve([])) } as unknown as Backend;
    const log = new LogController(api, immediateScheduler);
    log.setLevels(["error"]);
    await vi.waitFor(() => expect(log.store.getState().loading).toBe(false));
    expect(api.get_log).toHaveBeenLastCalledWith({ levels: ["error"], text: null, limit: 10_000 });
    log.append(entry(1, "information", "skip me"));
    log.append(entry(2, "error", "keep me"));
    expect(log.store.getState().entries.map((item) => item.message)).toEqual(["keep me"]);
  });
});
