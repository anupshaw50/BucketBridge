import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import { errorText } from "../api/errors";
import type { LogEntry, LogLevel } from "../api/types";
import { type FrameScheduler, FrameTask } from "./scheduler";

export const LOG_LEVELS: readonly LogLevel[] = ["error", "warning", "information"];

/** Entries kept in the panel: the newest 10,000, which is all the backend keeps in memory. */
export const LOG_LIMIT = 10_000;

const TEXT_DEBOUNCE_MS = 250;

export interface LogState {
  entries: LogEntry[];
  levels: LogLevel[];
  text: string;
  loading: boolean;
  error: string | null;
}

/**
 * Same matching the backend applies to get_log: the entry level is one of the
 * selected levels and the text, if any, occurs in the message or the source
 * (case-insensitive).
 */
export function matchesLogFilter(entry: LogEntry, levels: readonly LogLevel[], text: string): boolean {
  if (!levels.includes(entry.level)) {
    return false;
  }
  const needle = text.trim().toLocaleLowerCase();
  if (needle.length === 0) {
    return true;
  }
  return (
    entry.message.toLocaleLowerCase().includes(needle) || entry.source.toLocaleLowerCase().includes(needle)
  );
}

export class LogController {
  readonly store: StoreApi<LogState>;
  private request = 0;
  /** Live entries that arrived while get_log was in flight. */
  private buffered: LogEntry[] = [];
  /** Live entries waiting for the next frame. */
  private pending: LogEntry[] = [];
  private readonly frame: FrameTask;
  private textTimer: ReturnType<typeof setTimeout> | null = null;

  private readonly api: Backend;

  constructor(api: Backend, scheduler: FrameScheduler) {
    this.api = api;
    this.frame = new FrameTask(scheduler, () => this.flush());
    this.store = createStore<LogState>(() => ({
      entries: [],
      levels: [...LOG_LEVELS],
      text: "",
      loading: false,
      error: null,
    }));
  }

  async load(): Promise<void> {
    const { levels, text } = this.store.getState();
    const request = ++this.request;
    this.buffered = [];
    this.pending = [];
    if (levels.length === 0) {
      this.store.setState({ entries: [], loading: false, error: null });
      return;
    }
    this.store.setState({ loading: true, error: null });
    try {
      const allLevels = levels.length === LOG_LEVELS.length;
      const trimmed = text.trim();
      const entries = await this.api.get_log({
        levels: allLevels ? null : [...levels],
        text: trimmed.length > 0 ? trimmed : null,
        limit: LOG_LIMIT,
      });
      if (request !== this.request) {
        return;
      }
      const lastId = entries.reduce((max, entry) => Math.max(max, entry.id), -Infinity);
      const live = this.buffered.filter((entry) => entry.id > lastId);
      this.buffered = [];
      this.store.setState({ entries: [...entries, ...live].slice(-LOG_LIMIT), loading: false });
    } catch (error) {
      if (request === this.request) {
        this.buffered = [];
        this.store.setState({ loading: false, error: errorText(error) });
      }
    }
  }

  /** A `log://entry` event; matching entries are appended once per frame. */
  append(entry: LogEntry): void {
    const { levels, text, loading } = this.store.getState();
    if (!matchesLogFilter(entry, levels, text)) {
      return;
    }
    if (loading) {
      this.buffered.push(entry);
      return;
    }
    this.pending.push(entry);
    this.frame.request();
  }

  /** Applies pending live entries now. */
  flush(): void {
    this.frame.cancel();
    if (this.pending.length === 0) {
      return;
    }
    const { entries } = this.store.getState();
    let lastId = entries[entries.length - 1]?.id ?? -Infinity;
    const next = entries.slice();
    for (const entry of this.pending) {
      if (entry.id > lastId) {
        next.push(entry);
        lastId = entry.id;
      }
    }
    this.pending = [];
    this.store.setState({ entries: next.length > LOG_LIMIT ? next.slice(next.length - LOG_LIMIT) : next });
  }

  setLevels(levels: readonly LogLevel[]): void {
    this.store.setState({ levels: LOG_LEVELS.filter((level) => levels.includes(level)) });
    void this.load();
  }

  setText(text: string): void {
    this.store.setState({ text });
    if (this.textTimer !== null) {
      clearTimeout(this.textTimer);
    }
    this.textTimer = setTimeout(() => {
      this.textTimer = null;
      void this.load();
    }, TEXT_DEBOUNCE_MS);
  }

  async clear(): Promise<void> {
    await this.api.clear_log();
    this.request += 1;
    this.buffered = [];
    this.pending = [];
    this.store.setState({ entries: [], loading: false, error: null });
  }

  dispose(): void {
    if (this.textTimer !== null) {
      clearTimeout(this.textTimer);
      this.textTimer = null;
    }
    this.frame.cancel();
    this.request += 1;
  }
}
