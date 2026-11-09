import { act, render, screen, within } from "@testing-library/react";
import userEvent, { type UserEvent } from "@testing-library/user-event";
import type { CopyOptions, FileDropEvent, Platform, SavePathOptions } from "../api/platform";
import type { WindowLocation } from "../api/types";
import { App } from "../app/App";
import { tr } from "../i18n";
import { MockBackend, type MockOptions } from "../mock/mockBackend";
import { immediateScheduler } from "../state/scheduler";
import type { Notice } from "../state/status";
import { settle } from "./helpers";

/** Platform double: pickers answer from queues, side effects are recorded. */
export class TestPlatform implements Platform {
  readonly kind = "browser" as const;
  fileAnswers: (string[] | null)[] = [];
  folderAnswers: (string | null)[] = [];
  saveAnswers: (string | null)[] = [];
  readonly saveRequests: SavePathOptions[] = [];
  readonly opened: string[] = [];
  readonly revealed: string[] = [];
  readonly copied: string[] = [];
  readonly copiedSensitive: boolean[] = [];

  pickFiles(): Promise<string[] | null> {
    return Promise.resolve(this.fileAnswers.shift() ?? null);
  }

  pickFolder(): Promise<string | null> {
    return Promise.resolve(this.folderAnswers.shift() ?? null);
  }

  applicationAnswers: (string | null)[] = [];

  pickApplication(): Promise<string | null> {
    return Promise.resolve(this.applicationAnswers.shift() ?? null);
  }

  pickSavePath(options: SavePathOptions): Promise<string | null> {
    this.saveRequests.push(options);
    return Promise.resolve(this.saveAnswers.shift() ?? null);
  }

  openUrl(url: string): Promise<void> {
    this.opened.push(url);
    return Promise.resolve();
  }

  revealPath(path: string): Promise<void> {
    this.revealed.push(path);
    return Promise.resolve();
  }

  private fileDropHandlers = new Set<(event: FileDropEvent) => void>();

  onFileDrop(handler: (event: FileDropEvent) => void): () => void {
    this.fileDropHandlers.add(handler);
    return () => {
      this.fileDropHandlers.delete(handler);
    };
  }

  /** Test hook: a drag of files from the file manager. */
  dropFiles(event: FileDropEvent): void {
    for (const handler of [...this.fileDropHandlers]) {
      handler(event);
    }
  }

  copyText(text: string, options?: CopyOptions): Promise<void> {
    this.copied.push(text);
    this.copiedSensitive.push(options?.sensitive ?? false);
    return Promise.resolve();
  }
}

const live: MockBackend[] = [];

/** Stops the mock backends created by renderApp (called after each test). */
export function disposeRenderedApps(): void {
  while (live.length > 0) {
    live.pop()?.dispose();
  }
}

export interface RenderAppOptions {
  mock?: MockOptions;
  /** Runs against the backend before the app starts (stored settings, extra buckets, a waiting link). */
  prepare?: (mock: MockBackend) => void | Promise<void>;
  platform?: TestPlatform;
  /** Location the window starts at (a window opened with "Open in new window"). */
  startAt?: WindowLocation | null;
}

export interface RenderedApp {
  mock: MockBackend;
  platform: TestPlatform;
  /** Toast notifications the app tried to show. */
  notices: Notice[];
  user: UserEvent;
  /** Runs the simulated transfer/sync clock and lets the resulting events render. */
  advance: (ms: number) => Promise<void>;
  unmount: () => void;
}

/**
 * Renders the whole application against a fresh mock backend whose transfer
 * and sync engines only move when the test calls `advance`.
 */
export async function renderApp(options: RenderAppOptions = {}): Promise<RenderedApp> {
  const mock = new MockBackend({
    autoRun: false,
    latencyMs: 0,
    logObjects: 30,
    manyFiles: 0,
    ...options.mock,
  });
  live.push(mock);
  await options.prepare?.(mock);
  const platform = options.platform ?? new TestPlatform();
  const notices: Notice[] = [];
  const user = userEvent.setup();
  const view = render(
    <App
      services={{ backend: mock, platform, startAt: options.startAt ?? null }}
      env="test"
      notify={(notice) => notices.push(notice)}
      scheduler={immediateScheduler}
    />,
  );
  await act(async () => {
    await settle();
  });
  const advance = async (ms: number) => {
    await act(async () => {
      mock.advance(ms);
      await settle();
    });
  };
  return { mock, platform, notices, user, advance, unmount: view.unmount };
}

/** The status bar's message region. */
export function statusBar() {
  return within(screen.getByRole("status"));
}

/** Opens a bucket from the tree and waits for its listing. */
export async function openBucket(user: UserEvent, name: string): Promise<void> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
  const item = await within(tree).findByText(name);
  await user.click(item);
  await screen.findByRole("grid", { name: tr("Objects") });
}

/** The object table row whose name cell has exactly this text. */
export async function findObjectRow(name: string): Promise<HTMLElement> {
  const grid = await screen.findByRole("grid", { name: tr("Objects") });
  const cell = await within(grid).findByText(name, { selector: ".bb-col-name span" });
  const row = cell.closest<HTMLElement>('[role="row"]');
  if (row === null) {
    throw new Error(`no row for ${name}`);
  }
  return row;
}

export function queryObjectRow(name: string): HTMLElement | null {
  const grid = screen.queryByRole("grid", { name: tr("Objects") });
  if (grid === null) {
    return null;
  }
  const cell = within(grid).queryByText(name, { selector: ".bb-col-name span" });
  return cell?.closest<HTMLElement>('[role="row"]') ?? null;
}
