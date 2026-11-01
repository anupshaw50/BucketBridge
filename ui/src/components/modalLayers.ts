import { createContext } from "react";

/**
 * Tracks the order of open modals so that only the top-most one reacts to
 * Escape and outside clicks, and every new modal is stacked above the others.
 */
export class ModalLayers {
  private stack: readonly string[] = [];
  private readonly listeners = new Set<() => void>();

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  readonly getSnapshot = (): readonly string[] => this.stack;

  push(id: string): void {
    this.stack = [...this.stack.filter((item) => item !== id), id];
    this.notify();
  }

  remove(id: string): void {
    if (this.stack.includes(id)) {
      this.stack = this.stack.filter((item) => item !== id);
      this.notify();
    }
  }

  private notify(): void {
    for (const listener of this.listeners) {
      listener();
    }
  }
}

export const ModalLayersContext = createContext<ModalLayers | null>(null);

/** Base z-index of the first modal; each further modal goes two levels higher. */
export const MODAL_BASE_Z_INDEX = 300;
