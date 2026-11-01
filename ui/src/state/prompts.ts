import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import type { SessionPrompts } from "../api/guard";

export interface PromptState {
  unlockOpen: boolean;
  /** Account whose MFA code is being asked for; others wait in line. */
  mfaAccountId: string | null;
}

/** A successful MFA login this recent satisfies further prompts for the same account. */
const MFA_REUSE_MS = 10_000;

type Waiter = (ok: boolean) => void;

/**
 * Drives the unlock and MFA dialogs for the session guard. Concurrent
 * requests share one dialog: everyone waiting is released when it closes.
 */
export class PromptController implements SessionPrompts {
  readonly store: StoreApi<PromptState>;
  private unlockWaiters: Waiter[] = [];
  private readonly mfaWaiters = new Map<string, Waiter[]>();
  private readonly mfaSuccessAt = new Map<string, number>();

  private readonly raw: Backend;

  constructor(raw: Backend) {
    this.raw = raw;
    this.store = createStore<PromptState>(() => ({ unlockOpen: false, mfaAccountId: null }));
  }

  async requestUnlock(): Promise<boolean> {
    if (!this.store.getState().unlockOpen) {
      // Another prompt may have unlocked in the meantime: then just retry.
      try {
        const status = await this.raw.security_status();
        if (!status.master_password_set || status.unlocked) {
          return true;
        }
      } catch {
        // Fall through and ask; the dialog reports its own errors.
      }
    }
    return new Promise<boolean>((resolve) => {
      this.unlockWaiters.push(resolve);
      if (!this.store.getState().unlockOpen) {
        this.store.setState({ unlockOpen: true });
      }
    });
  }

  /** Opens the unlock dialog without a waiting command (startup, toolbar). */
  showUnlock(): void {
    if (!this.store.getState().unlockOpen) {
      this.store.setState({ unlockOpen: true });
    }
  }

  finishUnlock(ok: boolean): void {
    const waiters = this.unlockWaiters;
    this.unlockWaiters = [];
    this.store.setState({ unlockOpen: false });
    for (const waiter of waiters) {
      waiter(ok);
    }
  }

  requestMfa(accountId: string): Promise<boolean> {
    const last = this.mfaSuccessAt.get(accountId);
    if (last !== undefined && Date.now() - last < MFA_REUSE_MS && this.store.getState().mfaAccountId !== accountId) {
      return Promise.resolve(true);
    }
    return new Promise<boolean>((resolve) => {
      const waiters = this.mfaWaiters.get(accountId) ?? [];
      waiters.push(resolve);
      this.mfaWaiters.set(accountId, waiters);
      if (this.store.getState().mfaAccountId === null) {
        this.store.setState({ mfaAccountId: accountId });
      }
    });
  }

  finishMfa(ok: boolean): void {
    const accountId = this.store.getState().mfaAccountId;
    if (accountId === null) {
      return;
    }
    if (ok) {
      this.mfaSuccessAt.set(accountId, Date.now());
    }
    const waiters = this.mfaWaiters.get(accountId) ?? [];
    this.mfaWaiters.delete(accountId);
    const next = this.mfaWaiters.keys().next();
    this.store.setState({ mfaAccountId: next.done ? null : next.value });
    for (const waiter of waiters) {
      waiter(ok);
    }
  }
}
