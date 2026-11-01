import { createStore, type StoreApi } from "zustand/vanilla";
import type { FileFilter, Platform } from "../api/platform";
import { basename, dirname, type MockFs, normalizePath } from "./fs";

export type PickMode = "files" | "folder" | "save" | "reveal";

export interface PickRequest {
  mode: PickMode;
  title: string;
  startDir: string;
  /** Suggested file name (save mode). */
  fileName: string;
  /** Item to highlight (reveal mode). */
  highlight: string | null;
  filters: FileFilter[];
}

export interface PickerState {
  request: PickRequest | null;
}

/**
 * Pickers for the browser build: they browse the mock backend's in-memory
 * file system through an in-app dialog (MockFileChooser).
 */
export class MockPicker {
  readonly store: StoreApi<PickerState>;
  readonly fs: MockFs;
  readonly home: string;
  private resolve: ((value: string[] | null) => void) | null = null;

  constructor(fs: MockFs, home: string) {
    this.fs = fs;
    this.home = home;
    this.store = createStore<PickerState>(() => ({ request: null }));
  }

  open(request: PickRequest): Promise<string[] | null> {
    this.finish(null);
    return new Promise((resolve) => {
      this.resolve = resolve;
      this.store.setState({ request });
    });
  }

  finish(value: string[] | null): void {
    const resolve = this.resolve;
    this.resolve = null;
    this.store.setState({ request: null });
    resolve?.(value);
  }

  existingDir(path: string | undefined): string {
    if (path !== undefined) {
      const normalized = normalizePath(path);
      if (normalized !== null && this.fs.isDir(normalized)) {
        return normalized;
      }
    }
    return this.fs.isDir(this.home) ? this.home : "/";
  }
}

async function copyWithFallback(text: string): Promise<void> {
  // navigator.clipboard is missing outside secure contexts, whatever the DOM typings say.
  const clipboard = (navigator as Partial<Navigator>).clipboard;
  if (clipboard) {
    await clipboard.writeText(text);
    return;
  }
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  const copied = document.execCommand("copy");
  area.remove();
  if (!copied) {
    throw new Error("The browser refused to copy to the clipboard.");
  }
}

export function createBrowserPlatform(picker: MockPicker): Platform {
  return {
    kind: "browser",
    pickFiles: (title) =>
      picker.open({
        mode: "files",
        title,
        startDir: picker.existingDir(undefined),
        fileName: "",
        highlight: null,
        filters: [],
      }),
    pickFolder: async (title) => {
      const picked = await picker.open({
        mode: "folder",
        title,
        startDir: picker.existingDir(undefined),
        fileName: "",
        highlight: null,
        filters: [],
      });
      return picked?.[0] ?? null;
    },
    pickApplication: async (title) => {
      const picked = await picker.open({
        mode: "files",
        title,
        startDir: picker.existingDir(undefined),
        fileName: "",
        highlight: null,
        filters: [],
      });
      return picked?.[0] ?? null;
    },
    pickSavePath: async ({ title, defaultPath, filters }) => {
      const absolute = defaultPath.startsWith("/");
      const picked = await picker.open({
        mode: "save",
        title,
        startDir: picker.existingDir(absolute ? dirname(defaultPath) : undefined),
        fileName: absolute ? basename(defaultPath) : defaultPath,
        highlight: null,
        filters,
      });
      return picked?.[0] ?? null;
    },
    openUrl: (url) => {
      window.open(url, "_blank", "noopener,noreferrer");
      return Promise.resolve();
    },
    revealPath: async (path) => {
      const normalized = normalizePath(path);
      if (normalized === null || !picker.fs.exists(normalized)) {
        throw new Error(`Nothing exists at ${path} in the simulated file system.`);
      }
      await picker.open({
        mode: "reveal",
        title: "Show in folder",
        startDir: picker.fs.isDir(normalized) ? normalized : dirname(normalized),
        fileName: "",
        highlight: normalized,
        filters: [],
      });
    },
    copyText: copyWithFallback,
  };
}
