import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import { errorText } from "../api/errors";
import type { SavedFilter } from "../api/types";
import { tr } from "../i18n";
import type { BrowserController } from "./browser";
import type { StatusController } from "./status";

export interface FiltersState {
  /** The stored filters, in the order the backend keeps them. */
  saved: SavedFilter[];
  loaded: boolean;
  loading: boolean;
  error: string | null;
}

function sameName(left: string, right: string): boolean {
  return left.toLowerCase() === right.toLowerCase();
}

/**
 * The saved filters of the configuration. Applying one is the browser's job (`BrowserController.setFilter`);
 * this controller keeps the two in step when a filter is edited, renamed or deleted while it is applied.
 */
export class FilterController {
  readonly store: StoreApi<FiltersState>;

  private readonly api: Backend;
  private readonly browser: BrowserController;
  private readonly status: StatusController;
  private loading: Promise<void> | null = null;

  constructor(api: Backend, browser: BrowserController, status: StatusController) {
    this.api = api;
    this.browser = browser;
    this.status = status;
    this.store = createStore<FiltersState>(() => ({ saved: [], loaded: false, loading: false, error: null }));
  }

  get saved(): SavedFilter[] {
    return this.store.getState().saved;
  }

  /** Loads the filters once; later calls share the first attempt, or retry after a failure. */
  ensureLoaded(): Promise<void> {
    if (this.store.getState().loaded) {
      return Promise.resolve();
    }
    return this.reload();
  }

  /** Reads the filters again. A failure is kept in the state (the filter menu shows it), never thrown. */
  reload(): Promise<void> {
    this.loading ??= this.load().finally(() => {
      this.loading = null;
    });
    return this.loading;
  }

  private async load(): Promise<void> {
    this.store.setState({ loading: true });
    try {
      const saved = await this.api.list_filters();
      this.store.setState({ saved, loaded: true, loading: false, error: null });
    } catch (error) {
      this.store.setState({ loading: false, error: errorText(error) });
    }
  }

  /**
   * Adds a filter (`replaces` null) or replaces the one named `replaces`, renames included. Rejects with
   * the backend's error (`usage` for an invalid spec, `conflict` for a taken name).
   */
  async save(filter: SavedFilter, replaces: string | null): Promise<void> {
    const saved = await this.api.save_filter({ filter, replaces });
    this.store.setState({ saved, loaded: true, error: null });
    const active = this.browser.state.filter;
    if (active !== null && sameName(active.name, replaces ?? filter.name)) {
      const stored = saved.find((item) => sameName(item.name, filter.name));
      if (stored !== undefined) {
        this.browser.setFilter(stored);
      }
    }
  }

  async remove(name: string): Promise<void> {
    const saved = await this.api.delete_filter({ name });
    this.store.setState({ saved, loaded: true, error: null });
    const active = this.browser.state.filter;
    if (active !== null && sameName(active.name, name)) {
      this.browser.setFilter(null);
    }
  }

  /** Applies a stored filter to the object list; `false` when there is no such filter or it does not compile. */
  apply(name: string): boolean {
    const found = this.saved.find((item) => sameName(item.name, name));
    if (found === undefined) {
      this.status.error(tr("There is no saved filter named \"{name}\".", { name }));
      return false;
    }
    if (!this.browser.setFilter(found)) {
      return false;
    }
    this.status.info(tr("Showing only the items that match \"{name}\".", { name: found.name }));
    return true;
  }

  clearActive(): void {
    this.browser.setFilter(null);
  }
}
