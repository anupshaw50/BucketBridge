import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MockBackend } from "../../mock/mockBackend";
import { AppModel } from "../../state/model";
import { immediateScheduler } from "../../state/scheduler";
import { openBucket, renderApp, TestPlatform } from "../../test/renderApp";

async function accountId(mock: MockBackend): Promise<string> {
  const id = (await mock.list_accounts()).default_account_id;
  if (id === null) {
    throw new Error("no default account");
  }
  return id;
}

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: "Buckets and folders" });
  const row = (await within(tree).findByText(name)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${name}`);
  }
  return row;
}

describe("bookmarks (T14)", () => {
  it("adds a bucket bookmark from the bucket menu, opens it, renames it and deletes it", async () => {
    const { user, mock } = await renderApp();
    expect(screen.queryByRole("region", { name: "Bookmarks" })).toBeNull();

    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(await screen.findByRole("menuitem", { name: "Bookmark…" }));
    const dialog = await screen.findByRole("dialog", { name: "Add bookmark" });
    expect(within(dialog).getByText("s3://demo-photos/")).toBeTruthy();
    await user.type(within(dialog).getByRole("textbox", { name: "Name" }), "Holiday photos");
    await user.click(within(dialog).getByRole("button", { name: "Add bookmark" }));

    const section = await screen.findByRole("region", { name: "Bookmarks" });
    await user.click(within(section).getByRole("button", { name: "Holiday photos" }));
    await screen.findByRole("grid", { name: "Objects" });
    expect(await mock.list_bookmarks()).toHaveLength(1);

    await user.click(within(section).getByRole("button", { name: "Rename bookmark Holiday photos" }));
    const rename = await screen.findByRole("dialog", { name: "Rename bookmark" });
    const field = within(rename).getByRole<HTMLInputElement>("textbox", { name: "Name" });
    expect(field.value).toBe("Holiday photos");
    await user.clear(field);
    await user.type(field, "Photos");
    await user.click(within(rename).getByRole("button", { name: "Rename" }));
    await within(await screen.findByRole("region", { name: "Bookmarks" })).findByRole("button", { name: "Photos" });

    await user.click(screen.getByRole("button", { name: "Delete bookmark Photos" }));
    await user.click(await within(await screen.findByRole("dialog")).findByRole("button", { name: "Delete" }));
    await waitFor(() => expect(screen.queryByRole("region", { name: "Bookmarks" })).toBeNull());
    expect(await mock.list_bookmarks()).toEqual([]);
  });

  it("bookmarks a file and opens the folder that holds it", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const grid = await screen.findByRole("grid", { name: "Objects" });
    const first = within(grid).getAllByRole("row")[1];
    if (first === undefined) {
      throw new Error("no rows");
    }
    fireEvent.contextMenu(first);
    await user.click(await screen.findByRole("menuitem", { name: "Bookmark…" }));
    const dialog = await screen.findByRole("dialog", { name: "Add bookmark" });
    await user.click(within(dialog).getByRole("button", { name: "Add bookmark" }));
    await waitFor(async () => expect(await mock.list_bookmarks()).toHaveLength(1));
    const [saved] = await mock.list_bookmarks();
    expect(saved?.bucket).toBe("demo-photos");
    expect(saved?.label.startsWith("demo-photos/")).toBe(true);
  });

  it("shows an error for a location that is already bookmarked", async () => {
    const { user, mock } = await renderApp();
    await mock.add_bookmark({ account_id: await accountId(mock), bucket: "demo-photos", prefix: "", label: null });
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(await screen.findByRole("menuitem", { name: "Bookmark…" }));
    const dialog = await screen.findByRole("dialog", { name: "Add bookmark" });
    await user.click(within(dialog).getByRole("button", { name: "Add bookmark" }));
    expect(await within(dialog).findByText(/already bookmarked/)).toBeTruthy();
  });

  it("shows bookmarks added elsewhere (event) and drops them with their account", async () => {
    const { mock } = await renderApp();
    const id = await accountId(mock);
    await mock.add_bookmark({ account_id: id, bucket: "demo-photos", prefix: "", label: "From another window" });
    expect(await screen.findByRole("button", { name: "From another window" })).toBeTruthy();
    await mock.remove_account({ id });
    expect(await mock.list_bookmarks()).toEqual([]);
  });

  it("opens a new window from the bucket menu with the bucket as its location", async () => {
    const { user, mock } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(await screen.findByRole("menuitem", { name: "Open in new window" }));
    const id = await accountId(mock);
    await waitFor(() => expect(mock.openedWindows).toEqual([{ account_id: id, bucket: "demo-photos", prefix: "" }]));
  });

  it("starts at the location it was opened for", async () => {
    const mock = new MockBackend({ autoRun: false, latencyMs: 0, manyFiles: 0 });
    try {
      const id = await accountId(mock);
      const model = new AppModel({
        backend: mock,
        platform: new TestPlatform(),
        scheduler: immediateScheduler,
        startAt: { account_id: id, bucket: "demo-photos", prefix: "" },
      });
      const stop = model.start();
      await waitFor(() => expect(model.app.getState().ready).toBe(true));
      await waitFor(() => expect(model.browser.state.listing.bucket).toBe("demo-photos"));
      expect(model.session.store.getState().activeAccountId).toBe(id);
      stop();
    } finally {
      mock.dispose();
    }
  });

  it("ignores a start location of an account that does not exist", async () => {
    const mock = new MockBackend({ autoRun: false, latencyMs: 0, manyFiles: 0 });
    try {
      const model = new AppModel({
        backend: mock,
        platform: new TestPlatform(),
        scheduler: immediateScheduler,
        startAt: { account_id: "gone", bucket: "demo-photos", prefix: "" },
      });
      const stop = model.start();
      await waitFor(() => expect(model.app.getState().ready).toBe(true));
      expect(model.browser.state.listing.bucket).toBeNull();
      stop();
    } finally {
      mock.dispose();
    }
  });
});
