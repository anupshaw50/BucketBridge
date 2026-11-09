import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import { errorText } from "../api/errors";
import type { Account, AccountFolder, SecurityStatus } from "../api/types";

export interface SessionState {
  security: SecurityStatus | null;
  accounts: Account[];
  folders: AccountFolder[];
  defaultAccountId: string | null;
  activeAccountId: string | null;
  accountsLoaded: boolean;
  accountsError: string | null;
}

/** Accounts, the active account and the master-password state. */
export class SessionController {
  readonly store: StoreApi<SessionState>;
  private readonly activeListeners = new Set<(accountId: string | null) => void>();

  private readonly api: Backend;

  constructor(api: Backend) {
    this.api = api;
    this.store = createStore<SessionState>(() => ({
      security: null,
      accounts: [],
      folders: [],
      defaultAccountId: null,
      activeAccountId: null,
      accountsLoaded: false,
      accountsError: null,
    }));
  }

  /** Notified whenever the active account changes. */
  onActiveChange(listener: (accountId: string | null) => void): () => void {
    this.activeListeners.add(listener);
    return () => this.activeListeners.delete(listener);
  }

  activeAccount(): Account | null {
    const { accounts, activeAccountId } = this.store.getState();
    return accounts.find((account) => account.id === activeAccountId) ?? null;
  }

  accountById(id: string): Account | null {
    return this.store.getState().accounts.find((account) => account.id === id) ?? null;
  }

  setSecurity(security: SecurityStatus): void {
    this.store.setState({ security });
  }

  async refreshSecurity(): Promise<SecurityStatus> {
    const security = await this.api.security_status();
    this.store.setState({ security });
    return security;
  }

  /**
   * Reloads the account list. The active account is kept when it still
   * exists, otherwise the default (or first) account becomes active.
   */
  async refreshAccounts(): Promise<void> {
    try {
      const list = await this.api.list_accounts();
      const previous = this.store.getState().activeAccountId;
      const stillThere = list.accounts.some((account) => account.id === previous);
      const fallback =
        list.accounts.find((account) => account.id === list.default_account_id)?.id ?? list.accounts[0]?.id ?? null;
      const active = stillThere ? previous : fallback;
      this.store.setState({
        accounts: list.accounts,
        folders: list.folders,
        defaultAccountId: list.default_account_id,
        accountsLoaded: true,
        accountsError: null,
      });
      this.setActive(active);
    } catch (error) {
      this.store.setState({ accountsLoaded: true, accountsError: errorText(error) });
      throw error;
    }
  }

  setActive(accountId: string | null): void {
    if (this.store.getState().activeAccountId === accountId) {
      return;
    }
    this.store.setState({ activeAccountId: accountId });
    for (const listener of this.activeListeners) {
      listener(accountId);
    }
  }
}
