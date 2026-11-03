import { createStore, type StoreApi } from "zustand/vanilla";
import type { Backend } from "../api/backend";
import { withSessionGuard } from "../api/guard";
import type { Platform } from "../api/platform";
import type { AppInfo, WindowLocation } from "../api/types";
import { BookmarkController } from "./bookmarks";
import { mountSupported } from "../features/mount/support";
import { BrowserController } from "./browser";
import { ConflictStore } from "./conflicts";
import { DialogController } from "./dialogs";
import { EditController } from "./edits";
import { FilterController } from "./filters";
import { LogController } from "./log";
import { PromptController } from "./prompts";
import { animationFrameScheduler, type FrameScheduler } from "./scheduler";
import { SessionController } from "./session";
import { SettingsController } from "./settings";
import { type Notifier, StatusController } from "./status";
import { SyncController } from "./sync";
import { TransferStore } from "./transferStore";

export interface ClipboardContents {
  accountId: string;
  bucket: string;
  keys: string[];
  mode: "copy" | "cut";
}

export interface ClipboardState {
  contents: ClipboardContents | null;
}

export interface AppState {
  info: AppInfo | null;
  /** Initial loading finished (successfully or not). */
  ready: boolean;
}

export interface AppModelOptions {
  backend: Backend;
  platform: Platform;
  notify?: Notifier;
  scheduler?: FrameScheduler;
  /** Location this window opens at startup (a window opened with "Open in new window"). */
  startAt?: WindowLocation | null;
}

/**
 * Application state and behaviour, independent of React. Components read
 * the stores and call the controllers; the controllers call the backend.
 */
export class AppModel {
  /** Backend without the unlock/MFA recovery (used by the prompts themselves). */
  readonly raw: Backend;
  /** Backend whose commands recover from `locked` and `mfa_required` by prompting. */
  readonly api: Backend;
  readonly platform: Platform;
  readonly prompts: PromptController;
  readonly status: StatusController;
  readonly session: SessionController;
  readonly settings: SettingsController;
  readonly transfers: TransferStore;
  readonly conflicts: ConflictStore;
  readonly log: LogController;
  readonly browser: BrowserController;
  readonly dialogs: DialogController;
  readonly sync: SyncController;
  readonly filters: FilterController;
  readonly bookmarks: BookmarkController;
  readonly edits: EditController;
  readonly clipboard: StoreApi<ClipboardState>;
  readonly app: StoreApi<AppState>;
  private startGeneration = 0;
  private readonly startAt: WindowLocation | null;

  constructor(options: AppModelOptions) {
    const scheduler = options.scheduler ?? animationFrameScheduler;
    this.raw = options.backend;
    this.platform = options.platform;
    this.prompts = new PromptController(this.raw);
    this.api = withSessionGuard(this.raw, this.prompts);
    this.status = new StatusController(options.notify ?? (() => undefined));
    this.session = new SessionController(this.api);
    this.settings = new SettingsController(this.api);
    this.transfers = new TransferStore(scheduler);
    this.conflicts = new ConflictStore();
    this.log = new LogController(this.api, scheduler);
    this.browser = new BrowserController(this.api, this.session, this.settings, this.status);
    this.dialogs = new DialogController();
    this.sync = new SyncController(this.api, this.prompts, scheduler);
    this.filters = new FilterController(this.api, this.browser, this.status);
    this.bookmarks = new BookmarkController(this.api, this.session, this.browser);
    this.edits = new EditController(this.api, this.dialogs, this.status);
    this.startAt = options.startAt ?? null;
    this.clipboard = createStore<ClipboardState>(() => ({ contents: null }));
    this.app = createStore<AppState>(() => ({ info: null, ready: false }));
  }

  /** Subscribes to backend events and loads the initial state. Returns the cleanup. */
  start(): () => void {
    const generation = ++this.startGeneration;
    const api = this.raw;
    const cleanups: (() => void)[] = [
      api.listen("transfer://update", (task) => this.transfers.upsert(task)),
      api.listen("transfer://removed", (event) => this.transfers.remove(event.ids)),
      api.listen("transfer://conflict", (event) => this.conflicts.onEvent(event)),
      api.listen("sync://progress", (progress) => this.sync.onProgress(progress)),
      api.listen("sync://row", (event) => this.sync.onRow(event)),
      api.listen("sync://report", (report) => this.sync.onReport(report)),
      api.listen("sync_jobs://changed", (jobs) => this.sync.setJobs(jobs)),
      api.listen("log://entry", (entry) => this.log.append(entry)),
      api.listen("security://changed", (security) => this.session.setSecurity(security)),
      api.listen("bookmarks://changed", (items) => this.bookmarks.set(items)),
      api.listen("edit://changed", (session) => this.edits.onChanged(session)),
      this.transfers.onFlush((changed) => this.browser.onTasksChanged(changed)),
      this.session.onActiveChange((accountId) => {
        // A finished or never-run plan belongs to the previous account; a running one stays
        // tracked so its progress can still be followed and cancelled.
        if (!this.sync.state.running) {
          this.sync.reset();
        }
        // During startup the bootstrap opens the first account itself.
        if (this.app.getState().ready) {
          void this.browser.switchAccount(accountId);
        }
      }),
      this.settings.onChange((next, previous) => {
        if (next.interface.flat_browsing !== previous.interface.flat_browsing) {
          this.browser.applyFlat(next.interface.flat_browsing);
        }
      }),
    ];
    // Whether the build can mount drives decides if the bucket menu offers it.
    mountSupported(this);
    void this.edits.load();
    void this.bootstrap(generation);
    return () => {
      this.startGeneration += 1;
      for (const cleanup of cleanups) {
        cleanup();
      }
      this.browser.dispose();
      this.log.dispose();
    };
  }

  private async bootstrap(generation: number): Promise<void> {
    const alive = () => generation === this.startGeneration;
    const security = this.session.refreshSecurity().catch((error: unknown) => {
      this.status.error(error);
      return null;
    });
    // Settings first: the listing needs flat_browsing and the layout needs the panels.
    await this.settings.load().catch((error: unknown) => {
      this.status.error(error, { notify: "Settings could not be loaded" });
    });
    if (!alive()) {
      return;
    }
    const status = await security;
    if (!alive()) {
      return;
    }
    if (status !== null && status.master_password_set && !status.unlocked) {
      this.prompts.showUnlock();
    }
    void this.api
      .list_tasks()
      .then((tasks) => {
        if (alive()) {
          this.transfers.mergeSnapshot(tasks);
        }
      })
      .catch((error: unknown) => this.status.error(error));
    void this.api
      .list_conflicts()
      .then((pending) => {
        if (alive()) {
          this.conflicts.mergeSnapshot(pending);
        }
      })
      .catch((error: unknown) => this.status.error(error));
    void this.log.load();
    void this.filters.reload();
    void this.bookmarks.load();
    void this.sync.loadJobs();
    void this.api
      .app_info()
      .then((info) => {
        if (alive()) {
          this.app.setState({ info });
        }
      })
      .catch((error: unknown) => this.status.error(error));
    void this.api
      .last_crash_report()
      .then((report) => {
        if (alive() && report !== null) {
          this.dialogs.open({ kind: "crash", report });
        }
      })
      .catch((error: unknown) => this.status.error(error));
    try {
      await this.session.refreshAccounts();
    } catch (error) {
      this.status.error(error);
    }
    if (alive()) {
      await this.openStartLocation(alive);
      this.app.setState({ ready: true });
    }
  }

  /** Opens the account and folder this window was started for, or the active account's bucket list. */
  private async openStartLocation(alive: () => boolean): Promise<void> {
    const at = this.startAt;
    const known = at !== null && this.session.store.getState().accounts.some((account) => account.id === at.account_id);
    if (!known) {
      void this.browser.switchAccount(this.session.store.getState().activeAccountId);
      return;
    }
    this.session.setActive(at.account_id);
    await this.browser.switchAccount(at.account_id);
    if (alive()) {
      void this.browser.openLocation(at.bucket, at.prefix);
    }
  }
}
