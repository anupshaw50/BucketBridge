import { createElement, type ReactNode } from "react";
import type { Backend } from "../api/backend";
import type { Platform } from "../api/platform";
import { createBrowserPlatform, MockPicker } from "./browserPlatform";
import { MockBackend, type MockOptions } from "./mockBackend";
import { MockFileChooser } from "./MockFileChooser";
import { MOCK_HOME } from "./world";

export interface MockEnvironment {
  backend: Backend;
  platform: Platform;
  /** UI the browser platform needs: the chooser for the simulated file system. */
  platformUi: ReactNode;
  mock: MockBackend;
  picker: MockPicker;
}

/** Mock backend plus browser platform; what `pnpm dev` runs outside Tauri. */
export function createMockEnvironment(options: MockOptions = {}): MockEnvironment {
  const mock = new MockBackend(options);
  const picker = new MockPicker(mock.fs, MOCK_HOME);
  const platform = createBrowserPlatform(picker);
  return { backend: mock, platform, platformUi: createElement(MockFileChooser, { picker }), mock, picker };
}

/**
 * Options from the page URL, for trying different states in the browser:
 * `?mock=empty` (no accounts), `?mock=locked` (master password "correct horse"),
 * `?crash=1` (show a crash report), `?latency=0` (no artificial delay),
 * `?pageSize=50`, `?manyFiles=10000`.
 */
export function mockOptionsFromQuery(search: string): MockOptions {
  const params = new URLSearchParams(search);
  const options: MockOptions = {
    fixture: params.get("mock") === "empty" ? "empty" : "demo",
    latencyMs: 120,
    manyFiles: 10_000,
  };
  if (params.get("mock") === "locked") {
    options.masterPassword = "correct horse";
    options.locked = true;
  }
  if (params.get("crash") === "1") {
    options.crashReport = true;
  }
  const latency = Number(params.get("latency"));
  if (params.has("latency") && Number.isFinite(latency) && latency >= 0) {
    options.latencyMs = latency;
  }
  const pageSize = Number(params.get("pageSize"));
  if (params.has("pageSize") && Number.isInteger(pageSize) && pageSize >= 1 && pageSize <= 1000) {
    options.pageSize = pageSize;
  }
  const manyFiles = Number(params.get("manyFiles"));
  if (params.has("manyFiles") && Number.isInteger(manyFiles) && manyFiles >= 0 && manyFiles <= 50_000) {
    options.manyFiles = manyFiles;
  }
  return options;
}

export { MockBackend, type MockOptions } from "./mockBackend";
export { MOCK_CREDENTIALS, MOCK_HOME, MOCK_MFA_CODE } from "./world";
