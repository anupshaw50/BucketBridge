/** Schedules `callback` once; the returned function cancels it. */
export type FrameScheduler = (callback: () => void) => () => void;

/**
 * Runs the callback on the next animation frame, which batches bursts of
 * events into one store update per painted frame. Falls back to a 16 ms timer
 * where requestAnimationFrame is unavailable.
 */
export const animationFrameScheduler: FrameScheduler = (callback) => {
  if (typeof requestAnimationFrame === "function") {
    const handle = requestAnimationFrame(() => callback());
    return () => cancelAnimationFrame(handle);
  }
  const handle = setTimeout(callback, 16);
  return () => clearTimeout(handle);
};

/** Runs callbacks synchronously; used by unit tests that want immediate updates. */
export const immediateScheduler: FrameScheduler = (callback) => {
  callback();
  return () => undefined;
};

/**
 * A callback that runs at most once per scheduled frame, however often it is
 * requested. Safe with schedulers that run the callback synchronously.
 */
export class FrameTask {
  private readonly scheduler: FrameScheduler;
  private readonly run: () => void;
  private scheduled = false;
  private cancelScheduled: (() => void) | null = null;

  constructor(scheduler: FrameScheduler, run: () => void) {
    this.scheduler = scheduler;
    this.run = run;
  }

  request(): void {
    if (this.scheduled) {
      return;
    }
    this.scheduled = true;
    const cancel = this.scheduler(() => {
      this.scheduled = false;
      this.cancelScheduled = null;
      this.run();
    });
    // A synchronous scheduler has already run the callback; keep nothing then.
    if (this.isScheduled()) {
      this.cancelScheduled = cancel;
    }
  }

  isScheduled(): boolean {
    return this.scheduled;
  }

  cancel(): void {
    if (this.cancelScheduled !== null) {
      this.cancelScheduled();
      this.cancelScheduled = null;
    }
    this.scheduled = false;
  }
}
