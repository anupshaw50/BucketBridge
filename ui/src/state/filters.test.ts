import { afterEach, describe, expect, it } from "vitest";
import type { Backend } from "../api/backend";
import { BackendError } from "../api/errors";
import type { FilterSpec, SavedFilter } from "../api/types";
import { emptyFilterSpec } from "../lib/filters";
import { MockBackend } from "../mock/mockBackend";
import { BrowserController } from "./browser";
import { FilterController } from "./filters";
import { SessionController } from "./session";
import { SettingsController } from "./settings";
import { StatusController } from "./status";

const backends: MockBackend[] = [];

function setup(overrides: Partial<Backend> = {}) {
  const mock = new MockBackend({ autoRun: false, latencyMs: 0, logObjects: 30 });
  backends.push(mock);
  const api: Backend = Object.assign(Object.create(mock) as Backend, overrides);
  const status = new StatusController(() => undefined);
  const browser = new BrowserController(api, new SessionController(api), new SettingsController(api), status);
  const filters = new FilterController(api, browser, status);
  return { mock, api, status, browser, filters };
}

afterEach(() => {
  while (backends.length > 0) {
    backends.pop()?.dispose();
  }
});

function spec(patch: Partial<FilterSpec>): FilterSpec {
  return { ...emptyFilterSpec(), ...patch };
}

function saved(name: string, patch: Partial<FilterSpec> = {}): SavedFilter {
  return { name, filter: spec(patch) };
}

describe("FilterController", () => {
  it("loads the stored filters once and shares a load that is running", async () => {
    const { mock, filters } = setup();
    await mock.save_filter({ filter: saved("Logs", { prefix: "logs/" }), replaces: null });
    expect(filters.store.getState().loaded).toBe(false);
    await Promise.all([filters.ensureLoaded(), filters.reload()]);
    expect(filters.saved.map((item) => item.name)).toEqual(["Logs"]);
    expect(filters.store.getState()).toMatchObject({ loaded: true, loading: false, error: null });
    // Loaded filters are not read again by ensureLoaded, but reload picks up changes made elsewhere.
    await mock.save_filter({ filter: saved("Images"), replaces: null });
    await filters.ensureLoaded();
    expect(filters.saved).toHaveLength(1);
    await filters.reload();
    expect(filters.saved.map((item) => item.name)).toEqual(["Logs", "Images"]);
  });

  it("keeps a failed load in the state instead of throwing, and retries it", async () => {
    let failures = 1;
    const { mock, filters } = setup({
      list_filters: () => {
        if (failures-- > 0) {
          return Promise.reject(new BackendError("io", "config.toml: resource busy"));
        }
        return mock.list_filters();
      },
    });
    await filters.ensureLoaded();
    expect(filters.store.getState()).toMatchObject({ loaded: false, error: "config.toml: resource busy" });
    await filters.ensureLoaded();
    expect(filters.store.getState()).toMatchObject({ loaded: true, error: null });
  });

  it("applies a stored filter to the browser and says so", async () => {
    const { mock, filters, browser, status } = setup();
    await mock.save_filter({ filter: saved("Text", { glob: "*.txt" }), replaces: null });
    await filters.reload();
    expect(filters.apply("text")).toBe(true);
    expect(browser.state.filter).toMatchObject({ name: "Text", spec: spec({ glob: "*.txt" }) });
    expect(status.store.getState().message).toMatchObject({ level: "info", text: 'Showing only the items that match "Text".' });
    filters.clearActive();
    expect(browser.state.filter).toBeNull();
  });

  it("refuses to apply a filter that does not exist or does not compile", () => {
    const { filters, browser, status } = setup();
    expect(filters.apply("nothing")).toBe(false);
    expect(status.store.getState().message).toMatchObject({ level: "error", text: 'There is no saved filter named "nothing".' });
    expect(browser.setFilter(saved("Broken", { glob: "[a" }))).toBe(false);
    expect(browser.state.filter).toBeNull();
    expect(status.store.getState().message?.text).toContain("unclosed character class");
  });

  it("keeps the applied filter in step when it is replaced, renamed or deleted", async () => {
    const { filters, browser } = setup();
    await filters.save(saved("Text", { glob: "*.txt" }), null);
    expect(filters.apply("Text")).toBe(true);

    await filters.save(saved("Text", { glob: "*.md" }), "Text");
    expect(browser.state.filter).toMatchObject({ name: "Text", spec: spec({ glob: "*.md" }) });
    await filters.save(saved("Markdown", { glob: "*.md" }), "text");
    expect(browser.state.filter?.name).toBe("Markdown");
    expect(filters.saved.map((item) => item.name)).toEqual(["Markdown"]);

    // Another filter changing does not touch the applied one.
    await filters.save(saved("Other"), null);
    await filters.save(saved("Other", { prefix: "x/" }), "Other");
    expect(browser.state.filter?.name).toBe("Markdown");

    await filters.remove("Other");
    expect(browser.state.filter?.name).toBe("Markdown");
    await filters.remove("MARKDOWN");
    expect(browser.state.filter).toBeNull();
    expect(filters.saved).toEqual([]);
  });

  it("rejects with the backend's error and changes nothing", async () => {
    const { filters, browser } = setup();
    await filters.save(saved("Text"), null);
    await expect(filters.save(saved("TEXT"), null)).rejects.toMatchObject({ code: "conflict" });
    await expect(filters.save(saved("Bad", { glob: "[a" }), null)).rejects.toMatchObject({ code: "usage" });
    await expect(filters.remove("missing")).rejects.toMatchObject({ code: "not_found" });
    expect(filters.saved.map((item) => item.name)).toEqual(["Text"]);
    expect(browser.state.filter).toBeNull();
  });
});
