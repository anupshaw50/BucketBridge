import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import { errorText } from "../api/errors";
import type { Bookmark } from "../api/types";
import { isFolderKey, parentPrefix } from "../lib/s3keys";
import type { BrowserController } from "./browser";
import type { SessionController } from "./session";

export interface BookmarksState {
  items: Bookmark[];
  loaded: boolean;
  error: string | null;
}

/** The folder a bookmark opens: a bookmarked object opens the folder that holds it. */
export function bookmarkFolder(bookmark: Pick<Bookmark, "prefix">): string {
  return bookmark.prefix === "" || isFolderKey(bookmark.prefix) ? bookmark.prefix : parentPrefix(bookmark.prefix);
}

/**
 * The saved locations of the configuration. Every window keeps its own copy, in step through the
 * `bookmarks://changed` event, so a bookmark added in one window shows in all of them.
 */
export class BookmarkController {
  readonly store: StoreApi<BookmarksState>;

  private readonly api: Backend;
  private readonly session: SessionController;
  private readonly browser: BrowserController;

  constructor(api: Backend, session: SessionController, browser: BrowserController) {
    this.api = api;
    this.session = session;
    this.browser = browser;
    this.store = createStore<BookmarksState>(() => ({ items: [], loaded: false, error: null }));
  }

  /** Replaces the list (the event handler and the command results use this). */
  set(items: Bookmark[]): void {
    this.store.setState({ items, loaded: true, error: null });
  }

  /** Reads the list. A failure is kept in the state, never thrown. */
  async load(): Promise<void> {
    try {
      this.set(await this.api.list_bookmarks());
    } catch (error) {
      this.store.setState({ error: errorText(error) });
    }
  }

  /** Bookmarks a location; resolves the stored list and rejects with the backend error. */
  async add(accountId: string, bucket: string, prefix: string, label: string | null): Promise<void> {
    this.set(await this.api.add_bookmark({ account_id: accountId, bucket, prefix, label }));
  }

  async rename(id: string, label: string): Promise<void> {
    this.set(await this.api.rename_bookmark({ id, label }));
  }

  async remove(id: string): Promise<void> {
    this.set(await this.api.remove_bookmark({ id }));
  }

  /** Opens a bookmark in this window, in its account (which becomes the active one). */
  async open(bookmark: Bookmark): Promise<void> {
    // Changing the active account makes the browser load that account's buckets.
    this.session.setActive(bookmark.account_id);
    await this.browser.openLocation(bookmark.bucket, bookmarkFolder(bookmark));
  }
}
