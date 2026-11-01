import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import { toBbError } from "../api/errors";
import type { BbErrorCode, Bucket, FilterSpec, ObjectEntry, SavedFilter, TransferTask } from "../api/types";
import { type CompiledFilter, compileFilter, matchesObject } from "../lib/filters";
import {
  DEFAULT_SORT,
  filterEntries,
  isFolderEntry,
  nextSort,
  type SortColumn,
  type SortState,
  sortEntries,
} from "../lib/objects";
import { parentPrefix } from "../lib/s3keys";
import { EMPTY_SELECTION, prune, type Selection } from "../lib/selection";
import { touchesListing } from "../lib/transfers";
import type { SessionController } from "./session";
import type { SettingsController } from "./settings";
import type { StatusController } from "./status";

export type LoadStatus = "idle" | "loading" | "ready" | "error";

export interface BucketListState {
  accountId: string | null;
  status: LoadStatus;
  buckets: Bucket[];
  /** Names of the external buckets pinned to the account (they are part of `buckets`, with kind "external"). */
  pinned: string[];
  error: string | null;
  errorCode: BbErrorCode | null;
}

export interface ListingState {
  accountId: string | null;
  bucket: string | null;
  prefix: string;
  flat: boolean;
  /** State of the first page. */
  status: LoadStatus;
  /** A refresh is running while the previous entries stay visible. */
  refreshing: boolean;
  entries: ObjectEntry[];
  nextToken: string | null;
  error: string | null;
  errorCode: BbErrorCode | null;
  loadingMore: boolean;
  moreError: string | null;
}

export interface TreeNodeState {
  status: LoadStatus;
  prefixes: string[];
  nextToken: string | null;
  error: string | null;
  loadingMore: boolean;
}

/** A saved filter that narrows the object list (applied over the loaded entries, relative to the open folder). */
export interface ActiveFilter {
  name: string;
  spec: FilterSpec;
  compiled: CompiledFilter;
}

export interface BrowserState {
  bucketList: BucketListState;
  listing: ListingState;
  /** Loaded entries after the saved filter, the search filter and sort; what the table shows. */
  visible: ObjectEntry[];
  visibleKeys: string[];
  selection: Selection;
  sort: SortState;
  search: string;
  /** The saved filter in effect, kept while browsing folders and dropped when the account changes. */
  filter: ActiveFilter | null;
  tree: Record<string, TreeNodeState>;
  expanded: Record<string, boolean>;
}

/** Stops a refresh from walking an unbounded number of pages. */
const MAX_REFRESH_PAGES = 50;
const AUTO_REFRESH_DELAY_MS = 800;

export function treeKey(bucket: string, prefix: string): string {
  return JSON.stringify([bucket, prefix]);
}

function emptyListing(accountId: string | null, flat: boolean): ListingState {
  return {
    accountId,
    bucket: null,
    prefix: "",
    flat,
    status: "idle",
    refreshing: false,
    entries: [],
    nextToken: null,
    error: null,
    errorCode: null,
    loadingMore: false,
    moreError: null,
  };
}

function mergeEntries(existing: readonly ObjectEntry[], incoming: readonly ObjectEntry[]): ObjectEntry[] {
  const seen = new Set(existing.map((entry) => entry.key));
  const merged = existing.slice();
  for (const entry of incoming) {
    if (!seen.has(entry.key)) {
      seen.add(entry.key);
      merged.push(entry);
    }
  }
  return merged;
}

function folderPrefixes(entries: readonly ObjectEntry[]): string[] {
  return entries.filter((entry) => isFolderEntry(entry)).map((entry) => entry.key);
}

/** Bucket list, folder tree and the object listing of the active account. */
export class BrowserController {
  readonly store: StoreApi<BrowserState>;
  private bucketRequest = 0;
  private listRequest = 0;
  private readonly treeRequests = new Map<string, number>();
  private refreshTimer: ReturnType<typeof setTimeout> | null = null;
  /** Work touching the open folder finished while a refresh was running: refresh once more after it. */
  private refreshAgain = false;

  private readonly api: Backend;
  private readonly session: SessionController;
  private readonly settings: SettingsController;
  private readonly status: StatusController;

  constructor(api: Backend, session: SessionController, settings: SettingsController, status: StatusController) {
    this.api = api;
    this.session = session;
    this.settings = settings;
    this.status = status;
    this.store = createStore<BrowserState>(() => ({
      bucketList: { accountId: null, status: "idle", buckets: [], pinned: [], error: null, errorCode: null },
      listing: emptyListing(null, settings.current.interface.flat_browsing),
      visible: [],
      visibleKeys: [],
      selection: EMPTY_SELECTION,
      sort: DEFAULT_SORT,
      search: "",
      filter: null,
      tree: {},
      expanded: {},
    }));
  }

  get state(): BrowserState {
    return this.store.getState();
  }

  /** Clears everything and loads the buckets of another account. */
  async switchAccount(accountId: string | null): Promise<void> {
    this.cancelAutoRefresh();
    this.listRequest += 1;
    this.treeRequests.clear();
    this.store.setState({
      listing: emptyListing(accountId, this.settings.current.interface.flat_browsing),
      visible: [],
      visibleKeys: [],
      selection: EMPTY_SELECTION,
      search: "",
      filter: null,
      tree: {},
      expanded: {},
    });
    await this.loadBuckets(accountId);
  }

  async loadBuckets(accountId: string | null = this.session.store.getState().activeAccountId): Promise<void> {
    const request = ++this.bucketRequest;
    if (accountId === null) {
      this.store.setState({
        bucketList: { accountId: null, status: "idle", buckets: [], pinned: [], error: null, errorCode: null },
      });
      return;
    }
    const previous = this.state.bucketList;
    const same = previous.accountId === accountId;
    this.store.setState({
      bucketList: {
        accountId,
        status: "loading",
        buckets: same ? previous.buckets : [],
        pinned: same ? previous.pinned : [],
        error: null,
        errorCode: null,
      },
    });
    try {
      // The external buckets are only needed to tell which entries can be removed again; the
      // bucket list itself already contains them, so a failure to read them is not an error.
      const [buckets, pinned] = await Promise.all([
        this.api.list_buckets({ account_id: accountId }),
        this.api.list_external_buckets({ account_id: accountId }).catch((): string[] => []),
      ]);
      if (request !== this.bucketRequest) {
        return;
      }
      const sorted = [...buckets].sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
      this.store.setState({
        bucketList: { accountId, status: "ready", buckets: sorted, pinned, error: null, errorCode: null },
      });
    } catch (raw) {
      if (request !== this.bucketRequest) {
        return;
      }
      const error = toBbError(raw);
      this.store.setState({
        bucketList: {
          accountId,
          status: "error",
          buckets: [],
          pinned: [],
          error: error.message,
          errorCode: error.code,
        },
      });
      this.status.error(error);
    }
  }

  /** Opens a bucket at a prefix and loads the first page. */
  async openLocation(bucket: string, prefix: string): Promise<void> {
    const accountId = this.session.store.getState().activeAccountId;
    if (accountId === null) {
      return;
    }
    this.cancelAutoRefresh();
    const flat = this.settings.current.interface.flat_browsing;
    const request = ++this.listRequest;
    const sameLocation =
      this.state.listing.bucket === bucket &&
      this.state.listing.prefix === prefix &&
      this.state.listing.accountId === accountId;
    this.store.setState({
      listing: { ...emptyListing(accountId, flat), bucket, prefix, status: "loading" },
      search: sameLocation ? this.state.search : "",
      selection: EMPTY_SELECTION,
    });
    this.recompute();
    try {
      const page = await this.api.list_objects({ account_id: accountId, bucket, prefix, flat, token: null });
      if (request !== this.listRequest) {
        return;
      }
      this.store.setState({
        listing: {
          ...this.state.listing,
          status: "ready",
          entries: mergeEntries([], page.entries),
          nextToken: page.next_token,
        },
      });
      this.recompute();
      this.syncTreeWithListing();
    } catch (raw) {
      if (request !== this.listRequest) {
        return;
      }
      const error = toBbError(raw);
      this.store.setState({
        listing: { ...this.state.listing, status: "error", error: error.message, errorCode: error.code },
      });
      this.status.error(error);
    }
  }

  /** Reloads the current location, keeping the entries visible and as many pages as were loaded. */
  async refresh(): Promise<void> {
    const { listing } = this.state;
    if (listing.bucket === null || listing.accountId === null) {
      await this.loadBuckets();
      return;
    }
    if (listing.status !== "ready") {
      await this.openLocation(listing.bucket, listing.prefix);
      return;
    }
    if (this.refreshTimer !== null) {
      clearTimeout(this.refreshTimer);
      this.refreshTimer = null;
    }
    const request = ++this.listRequest;
    const { accountId, bucket, prefix, flat } = listing;
    const wanted = listing.entries.length;
    // A running "load more" is superseded by the refresh, which reloads those pages anyway.
    this.store.setState({ listing: { ...listing, refreshing: true, loadingMore: false, moreError: null } });
    try {
      let entries: ObjectEntry[] = [];
      let token: string | null = null;
      let pages = 0;
      do {
        const page = await this.api.list_objects({ account_id: accountId, bucket, prefix, flat, token });
        if (request !== this.listRequest) {
          return;
        }
        entries = mergeEntries(entries, page.entries);
        token = page.next_token;
        pages += 1;
      } while (token !== null && entries.length < wanted && pages < MAX_REFRESH_PAGES);
      this.store.setState({
        listing: {
          ...this.state.listing,
          refreshing: false,
          loadingMore: false,
          entries,
          nextToken: token,
          error: null,
          errorCode: null,
        },
      });
      this.recompute();
      this.syncTreeWithListing();
      this.afterRefresh();
    } catch (raw) {
      if (request !== this.listRequest) {
        return;
      }
      this.store.setState({ listing: { ...this.state.listing, refreshing: false } });
      this.status.error(raw);
      this.afterRefresh();
    }
  }

  async loadMore(): Promise<void> {
    const { listing } = this.state;
    if (
      listing.bucket === null ||
      listing.accountId === null ||
      listing.nextToken === null ||
      listing.loadingMore ||
      listing.refreshing ||
      listing.status !== "ready"
    ) {
      return;
    }
    const request = this.listRequest;
    const { accountId, bucket, prefix, flat, nextToken } = listing;
    this.store.setState({ listing: { ...listing, loadingMore: true, moreError: null } });
    try {
      const page = await this.api.list_objects({ account_id: accountId, bucket, prefix, flat, token: nextToken });
      if (request !== this.listRequest) {
        return;
      }
      const current = this.state.listing;
      this.store.setState({
        listing: {
          ...current,
          loadingMore: false,
          entries: mergeEntries(current.entries, page.entries),
          nextToken: page.next_token,
        },
      });
      this.recompute();
    } catch (raw) {
      if (request !== this.listRequest) {
        return;
      }
      const error = toBbError(raw);
      this.store.setState({ listing: { ...this.state.listing, loadingMore: false, moreError: error.message } });
      this.status.error(error);
    }
  }

  goUp(): void {
    const { bucket, prefix } = this.state.listing;
    if (bucket !== null && prefix !== "") {
      void this.openLocation(bucket, parentPrefix(prefix));
    }
  }

  setSort(column: SortColumn): void {
    this.store.setState({ sort: nextSort(this.state.sort, column) });
    this.recompute();
  }

  setSearch(search: string): void {
    this.store.setState({ search });
    this.recompute();
  }

  /**
   * Applies a saved filter to the loaded entries (`null` removes it). `false`, with the reason in the
   * status bar, when the filter does not compile.
   */
  setFilter(next: SavedFilter | null): boolean {
    if (next === null) {
      this.store.setState({ filter: null });
      this.recompute();
      return true;
    }
    const compiled = compileFilter(next.filter);
    if (!compiled.ok) {
      this.status.error(compiled.message, { notify: "The filter could not be applied" });
      return false;
    }
    this.store.setState({ filter: { name: next.name, spec: next.filter, compiled: compiled.filter } });
    this.recompute();
    return true;
  }

  setSelection(selection: Selection): void {
    this.store.setState({ selection });
  }

  selectedEntries(): ObjectEntry[] {
    const { visible, selection } = this.state;
    return visible.filter((entry) => selection.keys.has(entry.key));
  }

  /** Reacts to the flat-browsing setting by reloading the open location. */
  applyFlat(flat: boolean): void {
    const { listing } = this.state;
    if (listing.flat === flat) {
      return;
    }
    if (listing.bucket === null) {
      this.store.setState({ listing: { ...listing, flat } });
      return;
    }
    void this.openLocation(listing.bucket, listing.prefix);
  }

  /** Called with tasks whose state changed; refreshes once work touching the open folder ends. */
  onTasksChanged(tasks: readonly TransferTask[]): void {
    const { listing } = this.state;
    if (listing.bucket === null || listing.accountId === null || listing.status !== "ready") {
      return;
    }
    const relevant = tasks.some(
      (task) =>
        (task.state === "completed" || task.state === "failed" || task.state === "cancelled") &&
        touchesListing(task, listing.accountId ?? "", listing.bucket ?? "", listing.prefix),
    );
    if (relevant) {
      this.requestAutoRefresh();
    }
  }

  // ---- folder tree ----

  async toggleNode(bucket: string, prefix: string): Promise<void> {
    const key = treeKey(bucket, prefix);
    const expanded = !this.state.expanded[key];
    this.store.setState({ expanded: { ...this.state.expanded, [key]: expanded } });
    const node = this.state.tree[key];
    if (expanded && (node === undefined || node.status === "error")) {
      await this.loadTreeNode(bucket, prefix, false);
    }
  }

  async loadTreeNode(bucket: string, prefix: string, more: boolean): Promise<void> {
    const accountId = this.session.store.getState().activeAccountId;
    if (accountId === null) {
      return;
    }
    const key = treeKey(bucket, prefix);
    const existing = this.state.tree[key];
    if (more && (existing === undefined || existing.nextToken === null || existing.loadingMore)) {
      return;
    }
    const request = (this.treeRequests.get(key) ?? 0) + 1;
    this.treeRequests.set(key, request);
    const token = more ? (existing?.nextToken ?? null) : null;
    this.setTreeNode(
      key,
      more && existing
        ? { ...existing, loadingMore: true, error: null }
        : { status: "loading", prefixes: [], nextToken: null, error: null, loadingMore: false },
    );
    try {
      const page = await this.api.list_objects({ account_id: accountId, bucket, prefix, flat: false, token });
      if (this.treeRequests.get(key) !== request || this.session.store.getState().activeAccountId !== accountId) {
        return;
      }
      const base = more ? (this.state.tree[key]?.prefixes ?? []) : [];
      const prefixes = [...new Set([...base, ...folderPrefixes(page.entries)])];
      this.setTreeNode(key, {
        status: "ready",
        prefixes,
        nextToken: page.next_token,
        error: null,
        loadingMore: false,
      });
    } catch (raw) {
      if (this.treeRequests.get(key) !== request) {
        return;
      }
      const error = toBbError(raw);
      const current = this.state.tree[key];
      this.setTreeNode(
        key,
        more && current
          ? { ...current, loadingMore: false, error: error.message }
          : { status: "error", prefixes: [], nextToken: null, error: error.message, loadingMore: false },
      );
    }
  }

  /** Drops cached tree children of a bucket (after a refresh of the bucket list). */
  resetTree(): void {
    this.treeRequests.clear();
    this.store.setState({ tree: {}, expanded: {} });
  }

  dispose(): void {
    this.cancelAutoRefresh();
    this.listRequest += 1;
    this.bucketRequest += 1;
    this.treeRequests.clear();
  }

  private setTreeNode(key: string, node: TreeNodeState): void {
    this.store.setState({ tree: { ...this.state.tree, [key]: node } });
  }

  /** Keeps a loaded tree node in step with what the listing just showed for the same folder. */
  private syncTreeWithListing(): void {
    const { listing, tree } = this.state;
    if (listing.bucket === null || listing.flat) {
      return;
    }
    const key = treeKey(listing.bucket, listing.prefix);
    const node = tree[key];
    if (node?.status !== "ready") {
      return;
    }
    this.treeRequests.set(key, (this.treeRequests.get(key) ?? 0) + 1);
    this.setTreeNode(key, {
      status: "ready",
      prefixes: folderPrefixes(listing.entries),
      nextToken: listing.nextToken,
      error: null,
      loadingMore: false,
    });
  }

  private recompute(): void {
    const { listing, search, sort, selection, filter } = this.state;
    const scoped =
      filter === null
        ? listing.entries
        : listing.entries.filter((entry) => matchesObject(filter.compiled, entry, listing.prefix));
    const visible = sortEntries(filterEntries(scoped, search, listing.prefix), sort, listing.prefix);
    const visibleKeys = visible.map((entry) => entry.key);
    this.store.setState({ visible, visibleKeys, selection: prune(selection, visibleKeys) });
  }

  private cancelAutoRefresh(): void {
    if (this.refreshTimer !== null) {
      clearTimeout(this.refreshTimer);
      this.refreshTimer = null;
    }
    this.refreshAgain = false;
  }

  /**
   * Schedules an automatic refresh. A refresh in progress is never restarted
   * (that could starve it while tasks keep finishing); instead one more
   * refresh follows when it completes.
   */
  private requestAutoRefresh(): void {
    if (this.state.listing.refreshing) {
      this.refreshAgain = true;
      return;
    }
    this.refreshTimer ??= setTimeout(() => {
      this.refreshTimer = null;
      if (this.state.listing.refreshing) {
        this.refreshAgain = true;
      } else {
        void this.refresh();
      }
    }, AUTO_REFRESH_DELAY_MS);
  }

  /** Called when a refresh of the current location ends (successfully or not). */
  private afterRefresh(): void {
    if (this.refreshAgain) {
      this.refreshAgain = false;
      this.requestAutoRefresh();
    }
  }
}
