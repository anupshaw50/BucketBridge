import { createStore, type StoreApi } from "zustand/vanilla";
import { errorText } from "../api/errors";

export type StatusLevel = "error" | "info";

export interface StatusMessage {
  id: number;
  level: StatusLevel;
  text: string;
  at: number;
}

export interface StatusState {
  message: StatusMessage | null;
}

export interface Notice {
  level: StatusLevel;
  title: string;
  text: string;
}

export type Notifier = (notice: Notice) => void;

/** Status bar message (last error or information) and optional toast notifications. */
export class StatusController {
  readonly store: StoreApi<StatusState>;
  private sequence = 0;

  private readonly notifier: Notifier;

  constructor(notifier: Notifier) {
    this.notifier = notifier;
    this.store = createStore<StatusState>(() => ({ message: null }));
  }

  info(text: string, options: { notify?: string } = {}): void {
    this.set("info", text);
    if (options.notify !== undefined) {
      this.notifier({ level: "info", title: options.notify, text });
    }
  }

  /** Shows an error in the status bar; with `notify` also as a notification titled by it. */
  error(error: unknown, options: { notify?: string } = {}): void {
    const text = errorText(error);
    this.set("error", text);
    if (options.notify !== undefined) {
      this.notifier({ level: "error", title: options.notify, text });
    }
  }

  clear(): void {
    this.store.setState({ message: null });
  }

  private set(level: StatusLevel, text: string): void {
    this.sequence += 1;
    this.store.setState({ message: { id: this.sequence, level, text, at: Date.now() } });
  }
}
