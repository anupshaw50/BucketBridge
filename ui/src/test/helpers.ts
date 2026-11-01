import { vi } from "vitest";
import type { Backend, BackendEventName, BackendEvents } from "../api/backend";
import { MockBackend } from "../mock/mockBackend";

/** Lets queued microtasks and zero-delay timers run (mock events are delivered asynchronously). */
export function settle(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** Records every payload of one backend event. */
export function record<E extends BackendEventName>(backend: Backend, event: E): BackendEvents[E][] {
  const seen: BackendEvents[E][] = [];
  backend.listen(event, (payload) => seen.push(payload));
  return seen;
}

/**
 * Counts the commands the mock backend receives from now on (every snake_case
 * method: list_objects, enqueue_copy, ...). Returns the running total.
 */
export function countCommands(mock: MockBackend): () => number {
  const names = Object.getOwnPropertyNames(MockBackend.prototype).filter((name) => /^[a-z]+_[a-z_]+$/.test(name));
  const spies = names.map((name) =>
    vi.spyOn(mock as unknown as Record<string, (...args: unknown[]) => unknown>, name),
  );
  return () => spies.reduce((total, spy) => total + spy.mock.calls.length, 0);
}
