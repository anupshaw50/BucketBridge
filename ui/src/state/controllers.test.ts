import { afterEach, describe, expect, it, vi } from "vitest";
import type { Backend } from "../api/backend";
import { BackendError } from "../api/errors";
import type { ObjectEntry, ObjectPage, Settings, TransferTask } from "../api/types";
import { cloneSettings, DEFAULT_SETTINGS } from "../lib/settings";
import { BrowserController } from "./browser";
import { SessionController } from "./session";
import { SettingsController } from "./settings";
import { StatusController } from "./status";

function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("SettingsController.update", () => {
  it("never saves defaults over stored settings when they could not be loaded", async () => {
    const stored = cloneSettings(DEFAULT_SETTINGS);
    stored.proxy.host = "proxy.example.com";
    stored.advanced.list_page_size = 200;
    let failures = 2;
    const saved: Settings[] = [];
    const api = {
      get_settings: () =>
        failures-- > 0
          ? Promise.reject(new BackendError("io", "settings.json: resource busy"))
          : Promise.resolve(cloneSettings(stored)),
      save_settings: ({ settings }: { settings: Settings }) => {
        saved.push(settings);
        return Promise.resolve();
      },
    } as unknown as Backend;
    const settings = new SettingsController(api);
    await expect(settings.load()).rejects.toMatchObject({ code: "io" });
    // The toggle reloads first; while that fails too, nothing is saved.
    await expect(
      settings.update((draft) => {
        draft.interface.show_log_panel = true;
      }),
    ).rejects.toMatchObject({ code: "io" });
    expect(saved).toEqual([]);
    await settings.update((draft) => {
      draft.interface.show_log_panel = true;
    });
    expect(saved).toHaveLength(1);
    expect(saved[0]?.proxy.host).toBe("proxy.example.com");
    expect(saved[0]?.advanced.list_page_size).toBe(200);
    expect(saved[0]?.interface.show_log_panel).toBe(true);
  });

  it("applies quick successive updates on top of each other", async () => {
    const holds: (() => void)[] = [];
    const saved: Settings[] = [];
    const api = {
      get_settings: () => Promise.resolve(cloneSettings(DEFAULT_SETTINGS)),
      save_settings: ({ settings }: { settings: Settings }) =>
        new Promise<void>((resolve) => {
          holds.push(() => {
            saved.push(settings);
            resolve();
          });
        }),
    } as unknown as Backend;
    const settings = new SettingsController(api);
    await settings.load();
    const first = settings.update((draft) => {
      draft.interface.show_transfer_panel = false;
    });
    const second = settings.update((draft) => {
      draft.interface.show_log_panel = true;
    });
    await vi.waitFor(() => expect(holds).toHaveLength(1));
    holds.shift()?.();
    await first;
    await vi.waitFor(() => expect(holds).toHaveLength(1));
    holds.shift()?.();
    await second;
    expect(saved.at(-1)?.interface).toMatchObject({ show_transfer_panel: false, show_log_panel: true });
    expect(settings.current.interface).toMatchObject({ show_transfer_panel: false, show_log_panel: true });
  });
});

function entry(key: string): ObjectEntry {
  return {
    key,
    size: 1,
    etag: null,
    last_modified: null,
    storage_class: null,
    checksums: { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null },
    is_prefix: false,
  };
}

const finished: TransferTask = {
  id: "t",
  account_id: "acc",
  group_id: null,
  kind: "upload",
  src: { type: "local", path: "/x" },
  dst: { type: "s3", bucket: "bucket", key: "new.txt" },
  bytes_total: 1,
  bytes_done: 1,
  speed: 0,
  eta: null,
  state: "completed",
  error: null,
  attempts: 1,
  created_at: "2026-10-29T08:00:00Z",
  updated_at: "2026-10-29T08:00:00Z",
};

describe("BrowserController auto refresh", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("finishes refreshing and shows new objects while tasks keep completing", async () => {
    vi.useFakeTimers();
    let generation = 0;
    const api = {
      list_objects: (args: { token: string | null }) =>
        new Promise<ObjectPage>((resolve) => {
          setTimeout(() => {
            const page = Number(args.token ?? "0");
            const entries = [0, 1, 2].map((index) => entry(`g${generation}-p${page}-${index}.txt`));
            resolve({ entries, next_token: page < 9 ? String(page + 1) : null });
          }, 300);
        }),
    } as unknown as Backend;
    const session = new SessionController(api);
    session.store.setState({ activeAccountId: "acc" });
    const browser = new BrowserController(api, session, new SettingsController(api), new StatusController(() => undefined));
    const open = browser.openLocation("bucket", "");
    await vi.advanceTimersByTimeAsync(300);
    await open;
    for (let index = 0; index < 4; index += 1) {
      const more = browser.loadMore();
      await vi.advanceTimersByTimeAsync(300);
      await more;
    }
    expect(browser.state.listing.entries).toHaveLength(15);
    generation = 1;
    let sawNewObjects = false;
    for (let elapsed = 0; elapsed < 20_000; elapsed += 500) {
      browser.onTasksChanged([finished]);
      await vi.advanceTimersByTimeAsync(500);
      sawNewObjects ||= browser.state.listing.entries.some((item) => item.key.startsWith("g1"));
    }
    expect(sawNewObjects).toBe(true);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(browser.state.listing.refreshing).toBe(false);
    expect(browser.state.listing.entries).toHaveLength(15);
    expect(browser.state.listing.entries.every((item) => item.key.startsWith("g1"))).toBe(true);
  });

  it("runs one more refresh when tasks finish during a refresh", async () => {
    const pages: ReturnType<typeof deferred<ObjectPage>>[] = [];
    const api = {
      list_objects: () => {
        const next = deferred<ObjectPage>();
        pages.push(next);
        return next.promise;
      },
    } as unknown as Backend;
    const session = new SessionController(api);
    session.store.setState({ activeAccountId: "acc" });
    const browser = new BrowserController(api, session, new SettingsController(api), new StatusController(() => undefined));
    const open = browser.openLocation("bucket", "");
    pages[0]?.resolve({ entries: [entry("a.txt")], next_token: null });
    await open;
    vi.useFakeTimers();
    browser.onTasksChanged([finished]);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(pages).toHaveLength(2);
    expect(browser.state.listing.refreshing).toBe(true);
    browser.onTasksChanged([finished]);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(pages).toHaveLength(2);
    pages[1]?.resolve({ entries: [entry("a.txt"), entry("b.txt")], next_token: null });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(pages).toHaveLength(3);
    pages[2]?.resolve({ entries: [entry("a.txt"), entry("b.txt"), entry("c.txt")], next_token: null });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(browser.state.listing.entries.map((item) => item.key)).toEqual(["a.txt", "b.txt", "c.txt"]);
    expect(pages).toHaveLength(3);
  });
});
