import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { t, tr } from "../../i18n";
import { MockBackend } from "../../mock/mockBackend";
import { AppModel } from "../../state/model";
import { immediateScheduler } from "../../state/scheduler";
import { countCommands } from "../../test/helpers";
import { findObjectRow, openBucket, renderApp, statusBar, TestPlatform } from "../../test/renderApp";
import { isAvailable } from "./available";
import { bucketContext } from "./context";
import { ContextMenu, SUBMENU_DELAY_MS } from "./ContextMenu";
import type { ActionId } from "./ids";
import { ACTIONS } from "./registry";
import { type ResolvedItem, resolveMenu } from "./resolve";
import type { MenuState } from "./useContextMenu";

afterEach(() => {
  vi.useRealTimers();
});

// ---------------------------------------------------------------- helpers

/** Regular expression of a sentence without regex characters surprise. */
function textRe(sentence: string): RegExp {
  return new RegExp(sentence.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
}

/** The status message of one task of `what` queued. */
function taskAdded(what: string): string {
  return tr("{what}: 1 task added to the transfer queue.", { what: tr(what) });
}

/** Regular expression of the status message of several tasks of `what` queued. */
function tasksAdded(what: string): RegExp {
  const filled = textRe(tr("{what}: {n} tasks added to the transfer queue.", { what: tr(what), n: 0 }));
  return new RegExp(filled.source.replace(/0/g, "\\d+"));
}

/** Entries of an open menu as "Label (Shortcut)", with " [off]" when disabled. */
function entriesOf(name: string): string[] {
  const menu = screen.getByRole("menu", { name });
  return within(menu)
    .getAllByRole("menuitem")
    .map((item) => {
      const shortcut = item.querySelector(".bb-menu-shortcut")?.textContent;
      return `${item.getAttribute("data-label")}${shortcut ? ` (${shortcut})` : ""}${item.hasAttribute("disabled") ? " [off]" : ""}`;
    });
}

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
  const row = (await within(tree).findByText(name)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${name}`);
  }
  return row;
}

function item(name: string): HTMLElement {
  return screen.getByRole("menuitem", { name });
}

async function useAccount(user: UserEvent, label: string): Promise<void> {
  await user.click(screen.getByRole("button", { name: new RegExp(`^${textRe(tr("Account: {label}", { label: "" })).source}`) }));
  await user.click(await screen.findByRole("menuitem", { name: new RegExp(label) }));
}

const MAC_USER_AGENT = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15";

// ---------------------------------------------------------------- bucket menu

describe("bucket menu", () => {
  it("opens on a bucket with its entries, copies the name and the URL, and closes", async () => {
    const { user, platform } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    expect(entriesOf(tr("Bucket menu"))).toEqual([
      `${tr("Open")} (Enter)`,
      tr("Open in new window"),
      tr("New folder…"),
      tr("Upload files…"),
      tr("Upload folder…"),
      tr("Download all files…"),
      tr("Paste") + " [off]",
      tr("Properties"),
      tr("Sharing wizard…"),
      tr("Folder sync…"),
      tr("Sync jobs…"),
      tr("CloudFront"),
      tr("Copy bucket name"),
      tr("Copy https:// URL"),
      tr("Copy as"),
      tr("Open in browser"),
      tr("Bookmark…"),
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ]);
    await user.click(item(tr("Copy bucket name")));
    expect(platform.copied).toEqual(["demo-photos"]);
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(item(tr("Copy https:// URL")));
    expect(platform.copied.at(-1)).toBe("https://demo-photos.s3.us-east-1.amazonaws.com/");
  });

  it("keeps the same entries on an external endpoint and copies a path-style URL", async () => {
    const { user, platform } = await renderApp();
    await screen.findByText("demo-photos");
    await useAccount(user, "Archive");
    fireEvent.contextMenu(await bucketRow("archive-2025"));
    expect(entriesOf(tr("Bucket menu"))).toContain(tr("Copy https:// URL"));
    expect(entriesOf(tr("Bucket menu"))).toContain(tr("Folder sync…"));
    await user.click(item(tr("Copy https:// URL")));
    expect(platform.copied.at(-1)).toBe("https://storage.example.com/archive-2025");
  });

  it("opens the bucket and starts work in a bucket that is not open", async () => {
    const { user, mock, platform, advance } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-backups"));
    await user.click(item(tr("Open")));
    expect(
      await within(await screen.findByRole("navigation", { name: tr("Location") })).findByRole("button", { name: "demo-backups" }),
    ).toBeInTheDocument();

    // The menu of a bucket that is not open acts on that bucket.
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    platform.fileAnswers.push(["/home/demo/Documents/report.pdf"]);
    await user.click(item(tr("Upload files…")));
    expect(await statusBar().findByText(taskAdded("Upload"))).toBeInTheDocument();
    await advance(5000);
    expect(mock.objectKeys("demo-photos")).toContain("report.pdf");

    fireEvent.contextMenu(await bucketRow("demo-logs"));
    await user.click(item(tr("New folder…")));
    expect(await screen.findByText(tr("In {uri}", { uri: "s3://demo-logs/" }))).toBeInTheDocument();
  });

  it("queues every object of a bucket, or says that it is empty", async () => {
    const { user, platform } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-backups"));
    platform.folderAnswers.push("/home/demo/Downloads");
    await user.click(item(tr("Download all files…")));
    expect(await statusBar().findByText(tasksAdded("Download"))).toBeInTheDocument();

    fireEvent.contextMenu(await bucketRow("demo-empty"));
    await user.click(item(tr("Download all files…")));
    expect(await statusBar().findByText(tr("{bucket} is empty: there is nothing to download.", { bucket: "demo-empty" }))).toBeInTheDocument();
  });

  it("opens the sync dialog with the bucket chosen", async () => {
    const { user } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-backups"));
    await user.click(item(tr("Folder sync…")));
    const dialog = await screen.findByRole("dialog", { name: tr("Synchronise folders") });
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(await within(dialog).findByLabelText(tr("Selected S3 folder"))).toHaveTextContent("s3://demo-backups/");
  });

  it("deletes a bucket only after its name is typed", async () => {
    const { user, mock } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-empty"));
    await user.click(item(tr("Delete…")));
    // The buckets source shows this dialog's title and button in English.
    const dialog = await screen.findByRole("dialog", { name: tr("Delete bucket") });
    const confirm = within(dialog).getByRole("button", { name: tr("Delete bucket") });
    expect(confirm).toBeDisabled();
    await user.type(within(dialog).getByRole("textbox"), "demo-empty");
    await user.click(confirm);
    await waitFor(async () => expect((await mock.list_buckets({ account_id: await firstAccount(mock) })).map((b) => b.name)).not.toContain("demo-empty"));
  });

  it("acts on all marked buckets and hides what applies to a single one", async () => {
    const { user, platform } = await renderApp();
    await screen.findByText("demo-photos");
    await user.keyboard("{Control>}");
    await user.click(await bucketRow("demo-backups"));
    await user.click(await bucketRow("demo-photos"));
    await user.keyboard("{/Control}");
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    expect(entriesOf(tr("Bucket menu"))).toEqual([
      tr("Copy bucket name"),
      tr("Copy https:// URL"),
      tr("Copy as"),
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ]);
    await user.click(item(tr("Copy bucket name")));
    expect(platform.copied.at(-1)).toBe("demo-backups\ndemo-photos");

    // One typed confirmation for each bucket.
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(item(tr("Delete…")));
    expect(await screen.findAllByRole("dialog", { name: tr("Delete bucket") })).toHaveLength(2);
  });
});

async function firstAccount(mock: MockBackend): Promise<string> {
  return (await mock.list_accounts()).default_account_id ?? "";
}

// ---------------------------------------------------------------- folder and file menus

describe("folder menu", () => {
  it("offers the folder entries and runs them against the folder", async () => {
    const { user, mock, platform, advance } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("photos"));
    expect(entriesOf(tr("Folder menu"))).toEqual([
      `${tr("Open")} (Enter)`,
      tr("Open in new window"),
      tr("New folder…"),
      tr("Upload files…"),
      tr("Upload folder…"),
      tr("Download…"),
      `${tr("Copy")} (Ctrl+C)`,
      `${tr("Cut")} (Ctrl+X)`,
      `${tr("Paste")} (Ctrl+V) [off]`,
      tr("Copy all files…"),
      tr("Move all files…"),
      `${tr("Rename…")} (F2)`,
      tr("Folder sync…"),
      tr("Sync jobs…"),
      tr("Properties"),
      tr("Versions…"),
      tr("Search here"),
      tr("Filter…"),
      tr("Copy path"),
      tr("Copy as"),
      tr("Bookmark…"),
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ]);
    expect(item(tr("Paste"))).toHaveAttribute("title", tr("Copy or cut items first."));

    await user.click(item(tr("New folder…")));
    expect(await screen.findByText(tr("In {uri}", { uri: "s3://demo-photos/photos/" }))).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: tr("Cancel") }));

    fireEvent.contextMenu(await findObjectRow("photos"));
    platform.fileAnswers.push(["/home/demo/Documents/report.pdf"]);
    await user.click(item(tr("Upload files…")));
    expect(await statusBar().findByText(taskAdded("Upload"))).toBeInTheDocument();
    await advance(5000);
    expect(mock.objectKeys("demo-photos")).toContain("photos/report.pdf");

    fireEvent.contextMenu(await findObjectRow("photos"));
    await user.click(item(tr("Copy path")));
    expect(platform.copied.at(-1)).toBe("s3://demo-photos/photos/");

    fireEvent.contextMenu(await findObjectRow("photos"));
    await user.click(item(tr("Folder sync…")));
    const dialog = await screen.findByRole("dialog", { name: tr("Synchronise folders") });
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(await within(dialog).findByLabelText(tr("Selected S3 folder"))).toHaveTextContent("s3://demo-photos/photos/");
  });

  it("opens the folder and focuses the filter box for Search here", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("documents"));
    await user.click(item(tr("Search here")));
    await findObjectRow("notes.txt");
    expect(screen.getByRole("textbox", { name: tr("Filter loaded items") })).toHaveFocus();
  });

  it("asks before deleting a folder and queues the delete only when confirmed", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("archive"));
    await user.click(item(tr("Delete…")));
    const dialog = await screen.findByRole("dialog", { name: tr("Delete") });
    expect(
      within(dialog).getByText(textRe(tr("Delete {what} from {bucket}?", { what: tr("\"{key}\"", { key: "archive/" }), bucket: "demo-photos" }))),
    ).toBeInTheDocument();
    expect(within(dialog).getByText(textRe(tr("Folders are deleted together with everything inside them. This cannot be undone.")))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel") }));
    expect((await mock.list_tasks()).filter((task) => task.kind === "delete")).toEqual([]);

    fireEvent.contextMenu(await findObjectRow("archive"));
    await user.click(item(tr("Delete…")));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Delete") })).getByRole("button", { name: tr("Delete") }));
    expect(await statusBar().findByText(taskAdded("Delete"))).toBeInTheDocument();
  });
});

describe("file menu", () => {
  it("offers the file entries, selects the row first, and disables paste with a reason", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const row = await findObjectRow("readme.md");
    fireEvent.contextMenu(row);
    expect(entriesOf(tr("File menu"))).toEqual([
      `${tr("Open")} (Enter)`,
      tr("Open with…"),
      tr("Edit"),
      `${tr("Preview")} (F3)`,
      tr("Versions…"),
      tr("Download…"),
      `${tr("Copy")} (Ctrl+C)`,
      `${tr("Cut")} (Ctrl+X)`,
      `${tr("Paste")} (Ctrl+V) [off]`,
      tr("Move to…"),
      tr("Copy to…"),
      `${tr("Rename…")} (F2)`,
      tr("Metadata"),
      tr("ACL…"),
      tr("Share…"),
      tr("Copy as"),
      tr("Encrypt…"),
      tr("Decrypt…"),
      tr("Lock"),
      tr("Restore from Glacier…"),
      tr("Properties"),
      tr("Export…"),
      tr("Bookmark…"),
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ]);
    expect(row).toHaveAttribute("aria-selected", "true");
    expect(item(tr("Paste"))).toHaveAttribute("title", tr("Copy or cut items first."));
  });

  it("runs open, download, rename, properties and share through the existing actions", async () => {
    const { user, platform } = await renderApp();
    await openBucket(user, "demo-photos");

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Open")));
    expect(await statusBar().findByText(tr("Opening {key}.", { key: "readme.md" }))).toBeInTheDocument();

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    platform.folderAnswers.push("/home/demo/Downloads");
    await user.click(item(tr("Download…")));
    expect(await statusBar().findByText(taskAdded("Download"))).toBeInTheDocument();

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Rename…")));
    expect(await screen.findByRole("dialog", { name: tr("Rename object") })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: tr("Cancel") }));

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Properties")));
    expect(await screen.findByRole("dialog", { name: tr("Properties") })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: tr("Close") }));

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Share…")));
    expect(await screen.findByRole("dialog", { name: tr("Share link") })).toBeInTheDocument();
  });

  it("copies, pastes into a folder and cuts and moves: a clipboard round trip through the mock", async () => {
    const { user, mock, advance } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Copy")));
    expect(await statusBar().findByText(tr("1 item copied. Open the destination folder and paste."))).toBeInTheDocument();

    fireEvent.contextMenu(await findObjectRow("documents"));
    expect(item(tr("Paste"))).toBeEnabled();
    await user.click(item(tr("Paste")));
    expect(await statusBar().findByText(taskAdded("Copy"))).toBeInTheDocument();
    await advance(5000);
    expect(mock.objectKeys("demo-photos")).toEqual(expect.arrayContaining(["readme.md", "documents/readme.md"]));

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Cut")));
    fireEvent.contextMenu(await findObjectRow("archive"));
    await user.click(item(tr("Paste")));
    expect(await statusBar().findByText(taskAdded("Move"))).toBeInTheDocument();
    await advance(5000);
    expect(mock.objectKeys("demo-photos")).toContain("archive/readme.md");
    expect(mock.objectKeys("demo-photos")).not.toContain("readme.md");
    // The cut clipboard is spent: pasting is disabled again.
    fireEvent.contextMenu(await findObjectRow("documents"));
    expect(item(tr("Paste"))).toBeDisabled();
  });

  it("asks for confirmation before deleting and queues nothing when cancelled", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Delete…")));
    const dialog = await screen.findByRole("dialog", { name: tr("Delete") });
    expect(
      within(dialog).getByText(textRe(tr("Delete {what} from {bucket}?", { what: tr("\"{key}\"", { key: "readme.md" }), bucket: "demo-photos" }))),
    ).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel") }));
    expect((await mock.list_tasks()).filter((task) => task.kind === "delete")).toEqual([]);
  });
});

describe("selection menu", () => {
  it("shows what applies to every selected row, and the count and size when deleting", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("space folder"));
    await user.keyboard("{/Control}");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    expect(entriesOf(tr("Selection menu"))).toEqual([
      tr("Download…"),
      `${tr("Copy")} (Ctrl+C)`,
      `${tr("Cut")} (Ctrl+X)`,
      `${tr("Paste")} (Ctrl+V) [off]`,
      tr("Export…"),
      tr("Copy as"),
      `${tr("Select all")} (Ctrl+A)`,
      `${tr("Invert selection")} (Ctrl+I)`,
      `${tr("Refresh")} (F5)`,
      `${tr("Delete…")} (Del)`,
    ]);
    await user.click(item(tr("Delete…")));
    const dialog = await screen.findByRole("dialog", { name: tr("Delete") });
    expect(
      within(dialog).getByText(
        textRe(
          tr("Delete {what} from {bucket}?", {
            what: `${tr("{n} items", { n: 2 })}${tr(" ({size})", { size: "1.0 KiB" })}`,
            bucket: "demo-photos",
          }),
        ),
      ),
    ).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------- empty space and accounts

describe("empty-space menus", () => {
  it("offers the files-pane entries on the empty area and runs select all, invert and search", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const grid = screen.getByRole("grid", { name: tr("Objects") });
    fireEvent.contextMenu(grid);
    expect(entriesOf(tr("Objects menu"))).toEqual([
      `${tr("Paste")} (Ctrl+V) [off]`,
      tr("New folder…"),
      tr("Upload files…"),
      tr("Upload folder…"),
      `${tr("Select all")} (Ctrl+A)`,
      `${tr("Invert selection")} (Ctrl+I)`,
      `${tr("Refresh")} (F5)`,
      tr("Versions…"),
      tr("Filter…"),
      tr("Search"),
    ]);
    expect(item(tr("Paste"))).toHaveAttribute("title", tr("Copy or cut items first."));
    await user.click(item(tr("Select all")));
    expect(within(grid).queryAllByRole("row", { selected: true })).toHaveLength(7);
    fireEvent.contextMenu(grid);
    await user.click(item(tr("Invert selection")));
    expect(within(grid).queryAllByRole("row", { selected: true })).toHaveLength(0);
    // The list has focus when the menu opens; Search moves focus to the filter box and the menu must not take it back.
    act(() => grid.focus());
    fireEvent.contextMenu(grid);
    await user.click(item(tr("Search")));
    expect(screen.getByRole("textbox", { name: tr("Filter loaded items") })).toHaveFocus();
    fireEvent.contextMenu(grid);
    await user.click(item(tr("New folder…")));
    expect(await screen.findByRole("dialog", { name: tr("New folder") })).toBeInTheDocument();
  });

  it("offers new bucket and refresh on the empty area of the buckets pane", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    const pane = document.querySelector(".bb-tree-pane");
    expect(pane).not.toBeNull();
    fireEvent.contextMenu(pane!);
    expect(entriesOf(tr("Buckets menu"))).toEqual([
      tr("New bucket…"),
      tr("Add external bucket…"),
      `${tr("Refresh")} (F5)`,
      tr("IAM manager…"),
      tr("CloudFront manager…"),
      tr("Diagnostics…"),
      tr("Security scan…"),
      tr("Uncompleted multipart uploads…"),
    ]);
    const calls = vi.spyOn(mock, "list_buckets");
    await user.click(item(tr("Refresh")));
    await waitFor(() => expect(calls).toHaveBeenCalled());
    fireEvent.contextMenu(pane!);
    await user.click(item(tr("New bucket…")));
    // The buckets source shows this dialog's title in English.
    expect(await screen.findByRole("dialog", { name: tr("New bucket") })).toBeInTheDocument();
  });
});

describe("account menu", () => {
  async function openAccounts(user: UserEvent) {
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Accounts") }));
    return screen.findByRole("dialog", { name: tr("Accounts") });
  }

  it("offers the account entries on an account row, disabling set-as-default for the default account", async () => {
    const { user } = await renderApp();
    const manager = await openAccounts(user);
    fireEvent.contextMenu(within(manager).getByText("Demo"));
    expect(entriesOf(tr("Account menu"))).toEqual([
      tr("Edit account…"),
      tr("Set as default") + " [off]",
      tr("Change master password…"),
      tr("MFA device…"),
      tr("Test connection"),
      tr("Remove account…"),
    ]);
    await user.keyboard("{Escape}");
    // Escape closed the menu and left the dialog open.
    expect(screen.queryByRole("menu")).toBeNull();
    expect(screen.getByRole("dialog", { name: tr("Accounts") })).toBeInTheDocument();
  });

  it("sets the default, edits, focuses the MFA field, changes the master password and removes with a typed label", async () => {
    const { user, mock } = await renderApp();
    const manager = await openAccounts(user);
    const secure = (await mock.list_accounts()).accounts.find((account) => account.label === "Secure (MFA)");

    fireEvent.contextMenu(within(manager).getByText("Secure (MFA)"));
    await user.click(item(tr("Set as default")));
    await waitFor(async () => expect((await mock.list_accounts()).default_account_id).toBe(secure?.id));

    fireEvent.contextMenu(within(manager).getByText("Secure (MFA)"));
    await user.click(item(tr("MFA device…")));
    const edit = await screen.findByRole("dialog", { name: tr("Edit account {label}", { label: "Secure (MFA)" }) });
    await waitFor(() => expect(within(edit).getByRole("textbox", { name: new RegExp(tr("MFA device")) })).toHaveFocus());
    await user.click(within(edit).getByRole("button", { name: tr("Cancel") }));

    fireEvent.contextMenu(within(manager).getByText("Demo"));
    await user.click(item(tr("Edit account…")));
    expect(await screen.findByRole("dialog", { name: tr("Edit account {label}", { label: "Demo" }) })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: tr("Cancel") }));

    fireEvent.contextMenu(within(manager).getByText("Demo"));
    await user.click(item(tr("Change master password…")));
    expect(await screen.findByRole("dialog", { name: tr("Set a master password") })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: tr("Cancel") }));

    fireEvent.contextMenu(within(manager).getByText("Wrong secret"));
    await user.click(item(tr("Remove account…")));
    const confirm = await screen.findByRole("dialog", { name: tr("Remove account") });
    const remove = within(confirm).getByRole("button", { name: tr("Remove account") });
    expect(remove).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "Wrong secret");
    await user.click(remove);
    await waitFor(async () =>
      expect((await mock.list_accounts()).accounts.map((account) => account.label)).not.toContain("Wrong secret"),
    );
  });

  it("opens from the account button of the toolbar for the active account", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    fireEvent.contextMenu(screen.getByRole("button", { name: tr("Account: {label}", { label: "Demo" }) }));
    expect(entriesOf(tr("Account menu"))).toContain(tr("Edit account…"));
    await user.click(item(tr("Edit account…")));
    expect(await screen.findByRole("dialog", { name: tr("Edit account {label}", { label: "Demo" }) })).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------- keyboard, pointer and placement

describe("keyboard use of an open menu", () => {
  it("moves with the arrow keys, Home and End, skips disabled entries, and wraps", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    expect(document.activeElement).toHaveAccessibleName(tr("Open"));
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toHaveAccessibleName(tr("Open with…"));
    await user.keyboard("{ArrowUp}{ArrowUp}");
    expect(document.activeElement).toHaveAccessibleName(tr("Delete…"));
    await user.keyboard("{Home}");
    expect(document.activeElement).toHaveAccessibleName(tr("Open"));
    await user.keyboard("{End}");
    expect(document.activeElement).toHaveAccessibleName(tr("Delete…"));
    // Paste is disabled (empty clipboard), so the arrow keys step over it.
    await user.keyboard("{Home}{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}");
    expect(document.activeElement).toHaveAccessibleName(tr("Rename…"));
  });

  it("jumps to an entry by typing its first letters and runs it with Enter", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    // The first letters of "Properties" (in the language of the interface).
    await user.keyboard(tr("Properties").slice(0, 4));
    expect(document.activeElement).toHaveAccessibleName(tr("Properties"));
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("dialog", { name: tr("Properties") })).toBeInTheDocument();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("treats a space inside typed text as part of it instead of running the entry", async () => {
    const { user, platform } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    // The prefix before and after one of its own spaces, so that the typed space is in the middle.
    const label = tr("Copy https:// URL");
    const middle = label.indexOf(" ");
    await user.keyboard(label.slice(0, middle));
    await user.keyboard(" ");
    await user.keyboard(label.slice(middle + 1, middle + 2));
    expect(document.activeElement).toHaveAccessibleName(label);
    expect(screen.getByRole("menu", { name: tr("Bucket menu") })).toBeInTheDocument();
    expect(platform.copied).toEqual([]);
    await user.keyboard("{Enter}");
    expect(platform.copied).toEqual(["https://demo-photos.s3.us-east-1.amazonaws.com/"]);
  });

  it("closes on Escape and gives focus back to the list", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const grid = screen.getByRole("grid", { name: tr("Objects") });
    act(() => grid.focus());
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    expect(screen.getByRole("menu", { name: tr("File menu") })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(grid).toHaveFocus();
  });

  it("opens from the keyboard with the Menu key and Shift+F10", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{ContextMenu}");
    expect(screen.getByRole("menu", { name: tr("File menu") })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("{Shift>}{F10}{/Shift}");
    expect(screen.getByRole("menu", { name: tr("File menu") })).toBeInTheDocument();
  });

  it("closes when the window is scrolled or loses focus, but not when the menu itself scrolls", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    fireEvent.scroll(screen.getByRole("menu", { name: tr("File menu") }));
    expect(screen.getByRole("menu", { name: tr("File menu") })).toBeInTheDocument();
    fireEvent.scroll(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    act(() => {
      window.dispatchEvent(new Event("blur"));
    });
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
    await user.keyboard("{Escape}");
  });
});

describe("Ctrl+click", () => {
  it("opens the menu on macOS, without toggling the selection", async () => {
    vi.spyOn(window.navigator, "userAgent", "get").mockReturnValue(MAC_USER_AGENT);
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("photos"));
    await user.keyboard("{/Control}");
    expect(screen.getByRole("menu", { name: tr("Folder menu") })).toBeInTheDocument();
    // The click opened a menu instead of adding the folder to the selection.
    expect(await findObjectRow("readme.md")).not.toHaveAttribute("aria-selected", "true");
    await user.keyboard("{Escape}");

    await user.keyboard("{Control>}");
    await user.click(screen.getByRole("grid", { name: tr("Objects") }));
    await user.keyboard("{/Control}");
    expect(screen.getByRole("menu", { name: tr("Objects menu") })).toBeInTheDocument();
    await user.keyboard("{Escape}");

    await user.keyboard("{Control>}");
    await user.click(await bucketRow("demo-backups"));
    await user.keyboard("{/Control}");
    expect(screen.getByRole("menu", { name: tr("Bucket menu") })).toBeInTheDocument();
  });

  it("does not open when the pointer was dragged, and keeps its selection meaning elsewhere", async () => {
    vi.spyOn(window.navigator, "userAgent", "get").mockReturnValue(MAC_USER_AGENT);
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const row = await findObjectRow("readme.md");
    fireEvent.pointerDown(row, { button: 0, ctrlKey: true, clientX: 10, clientY: 10 });
    fireEvent.click(row, { button: 0, ctrlKey: true, clientX: 90, clientY: 60 });
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.pointerDown(row, { button: 0, ctrlKey: true, clientX: 10, clientY: 10 });
    fireEvent.click(row, { button: 0, ctrlKey: true, clientX: 12, clientY: 11 });
    expect(screen.getByRole("menu", { name: tr("File menu") })).toBeInTheDocument();
  });

  it("keeps Ctrl+click as add-to-selection on other systems", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("photos"));
    await user.keyboard("{/Control}");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(await findObjectRow("photos")).toHaveAttribute("aria-selected", "true");
  });
});

describe("placement", () => {
  it("keeps the menu inside the window", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const box = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockReturnValue({ width: 220, height: 300, top: 0, left: 0, right: 220, bottom: 300, x: 0, y: 0, toJSON: () => ({}) });
    fireEvent.contextMenu(await findObjectRow("readme.md"), { clientX: 5000, clientY: 5000 });
    const menu = screen.getByRole("menu", { name: tr("File menu") });
    expect(menu.style.left).toBe(`${window.innerWidth - 220 - 4}px`);
    expect(menu.style.top).toBe(`${window.innerHeight - 300 - 4}px`);
    box.mockRestore();
  });
});

// ---------------------------------------------------------------- submenus

/** Property entries that are available in the build; the submenu fixture below keeps only two of them. */
const OTHER_SETTINGS: ReadonlySet<ActionId> = new Set<ActionId>([
  "bucket.logging",
  "bucket.public-access",
  "bucket.website",
  "bucket.website-routing",
  "bucket.acceleration",
  "bucket.cors",
  "bucket.lifecycle",
  "bucket.tiering",
  "bucket.replication",
]);

describe("submenus", () => {
  /** A bucket menu in which a configuration feature has landed, so that a submenu exists. */
  async function menuWithSubmenu(): Promise<{ state: MenuState; run: ReturnType<typeof vi.fn> }> {
    const mock = new MockBackend({ autoRun: false, latencyMs: 0, manyFiles: 0 });
    const model = new AppModel({ backend: mock, platform: new TestPlatform(), scheduler: immediateScheduler });
    const stop = model.start();
    await vi.waitFor(() => expect(model.browser.state.bucketList.status).toBe("ready"));
    const run = vi.fn();
    const bucket = model.browser.state.bucketList.buckets[0];
    if (bucket === undefined) {
      throw new Error("no bucket");
    }
    const items: ResolvedItem[] = resolveMenu("bucket", bucketContext(model, [bucket]), {
      defs: {
        ...ACTIONS,
        "bucket.versioning": { run },
        "bucket.policy": { run },
      },
      available: (id: ActionId) => (isAvailable(id) && !OTHER_SETTINGS.has(id)) || id === "bucket.versioning" || id === "bucket.policy",
    });
    stop();
    mock.dispose();
    return { state: { id: 1, x: 10, y: 10, items, label: t("menu.bucket"), returnFocus: null }, run };
  }

  it("opens after a hover delay, closes when another entry is hovered, and runs its entries", async () => {
    const { state, run } = await menuWithSubmenu();
    vi.useFakeTimers();
    render(<ContextMenu menu={state} onClose={() => undefined} />);
    const trigger = screen.getByRole("menuitem", { name: tr("Properties") });
    expect(trigger).toHaveAttribute("aria-haspopup", "menu");
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    fireEvent.mouseEnter(trigger.closest(".bb-menu-entry")!);
    act(() => {
      vi.advanceTimersByTime(SUBMENU_DELAY_MS - 1);
    });
    expect(screen.queryByRole("menu", { name: tr("Properties") })).toBeNull();
    act(() => {
      vi.advanceTimersByTime(1);
    });
    const submenuElement = screen.getByRole("menu", { name: tr("Properties") });
    expect(within(submenuElement).getAllByRole("menuitem").map((entry) => entry.getAttribute("data-label"))).toEqual([
      tr("Versioning…"),
      tr("Bucket policy…"),
      tr("Object ownership…"),
      tr("Object lock…"),
    ]);
    expect(trigger).toHaveAttribute("aria-expanded", "true");

    fireEvent.mouseEnter(screen.getByRole("menuitem", { name: tr("Refresh") }).closest(".bb-menu-entry")!);
    expect(screen.queryByRole("menu", { name: tr("Properties") })).toBeNull();

    fireEvent.mouseEnter(trigger.closest(".bb-menu-entry")!);
    act(() => {
      vi.advanceTimersByTime(SUBMENU_DELAY_MS);
    });
    fireEvent.click(within(screen.getByRole("menu", { name: tr("Properties") })).getByRole("menuitem", { name: tr("Versioning…") }));
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("opens with ArrowRight, closes with ArrowLeft or Escape, and Escape on the top level closes the menu", async () => {
    const { state } = await menuWithSubmenu();
    const onClose = vi.fn();
    const { user } = await renderApp();
    expect(user).toBeDefined();
    render(<ContextMenu menu={state} onClose={onClose} />);
    const trigger = screen.getByRole("menuitem", { name: tr("Properties") });
    act(() => trigger.focus());
    fireEvent.keyDown(trigger, { key: "ArrowRight" });
    const submenuElement = screen.getByRole("menu", { name: tr("Properties") });
    expect(document.activeElement).toHaveAccessibleName(tr("Versioning…"));
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(document.activeElement).toHaveAccessibleName(tr("Bucket policy…"));
    fireEvent.keyDown(document.activeElement!, { key: "ArrowLeft" });
    expect(screen.queryByRole("menu", { name: tr("Properties") })).toBeNull();
    expect(trigger).toHaveFocus();
    expect(submenuElement).not.toBeInTheDocument();

    fireEvent.keyDown(trigger, { key: "Enter" });
    expect(screen.getByRole("menu", { name: tr("Properties") })).toBeInTheDocument();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: tr("Properties") })).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(trigger, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

// ---------------------------------------------------------------- no network, no dead entries

describe("menus and the backend", () => {
  it("performs no backend call while any of the menus is open", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{Control>}c{/Control}");
    const total = countCommands(mock);
    const opened: (() => void)[] = [
      () => fireEvent.contextMenu(screen.getByRole("grid", { name: tr("Objects") })),
      () => fireEvent.contextMenu(document.querySelector(".bb-tree-pane")!),
    ];
    for (const open of opened) {
      open();
      expect(screen.getByRole("menu")).toBeInTheDocument();
      await user.keyboard("{ArrowDown}{ArrowDown}");
      await user.keyboard("{Escape}");
    }
    const rows = ["readme.md", "photos"];
    for (const name of rows) {
      fireEvent.contextMenu(await findObjectRow(name));
      await user.keyboard("{Escape}");
    }
    fireEvent.contextMenu(await bucketRow("demo-backups"));
    await user.keyboard("{Escape}");
    fireEvent.contextMenu(screen.getByRole("button", { name: tr("Account: {label}", { label: "Demo" }) }));
    await user.keyboard("{Escape}");
    expect(total()).toBe(0);
  });
});

// ---------------------------------------------------------------- shortcut parity

/** Every shortcut a menu shows, as "action@pane". A shortcut added to a menu without an entry here fails the last test. */
const PARITY = [
  "open@files",
  "item.rename@files",
  "item.delete@files",
  "clipboard.copy@files",
  "clipboard.cut@files",
  "clipboard.paste@files",
  "list.select-all@files",
  "list.invert@files",
  "list.refresh@files",
  "file.preview@files",
  "bucket.open@buckets",
  "open@buckets",
  "bucket.delete@buckets",
  "bucket.refresh@buckets",
] as const;

describe("shortcut parity", () => {
  it("makes every shortcut of the files pane work without opening a menu", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const grid = screen.getByRole("grid", { name: tr("Objects") });
    const selected = () => within(grid).queryAllByRole("row", { selected: true }).length;

    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{F2}"); // item.rename
    await user.click(await screen.findByRole("button", { name: tr("Cancel") }));
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{F3}"); // file.preview
    await user.click(
      within(await screen.findByRole("dialog", { name: textRe(tr("Preview: {name}", { name: "readme.md" })) })).getByRole("button", {
        name: tr("Close"),
      }),
    );
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{Control>}c{/Control}"); // clipboard.copy
    expect(await statusBar().findByText(tr("1 item copied. Open the destination folder and paste."))).toBeInTheDocument();
    await user.keyboard("{Control>}x{/Control}"); // clipboard.cut
    expect(await statusBar().findByText(tr("1 item cut. Open the destination folder and paste."))).toBeInTheDocument();
    await user.click(await findObjectRow("documents"));
    await user.keyboard("{Enter}"); // open
    await findObjectRow("notes.txt");
    await user.keyboard("{Control>}v{/Control}"); // clipboard.paste
    expect(await statusBar().findByText(taskAdded("Move"))).toBeInTheDocument();

    await user.keyboard("{Control>}a{/Control}"); // list.select-all
    expect(selected()).toBeGreaterThan(1);
    await user.keyboard("{Control>}i{/Control}"); // list.invert
    expect(selected()).toBe(0);

    await user.click(await findObjectRow("notes.txt"));
    await user.keyboard("{Delete}"); // item.delete
    await user.click(within(await screen.findByRole("dialog", { name: tr("Delete") })).getByRole("button", { name: tr("Cancel") }));

    const listed = vi.spyOn(mock, "list_objects");
    await user.keyboard("{F5}"); // list.refresh
    await waitFor(() => expect(listed).toHaveBeenCalled());
  });

  it("makes every shortcut of the buckets pane work without opening a menu", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    const location = async (name: string) =>
      within(await screen.findByRole("navigation", { name: tr("Location") })).findByRole("button", { name });
    const treeRow = (name: string) => {
      const tree = screen.getByRole("tree", { name: tr("Buckets and folders") });
      return within(tree).getByText(name).closest<HTMLElement>('[role="treeitem"]');
    };

    act(() => treeRow("demo-empty")?.focus());
    await user.keyboard("{Enter}"); // bucket.open
    expect(await location("demo-empty")).toBeInTheDocument();

    const listed = vi.spyOn(mock, "list_buckets");
    await user.keyboard("{F5}"); // bucket.refresh
    await waitFor(() => expect(listed).toHaveBeenCalled());

    act(() => treeRow("demo-empty")?.focus());
    await user.keyboard("{Delete}"); // bucket.delete
    // The buckets source shows this dialog's title in English.
    expect(await screen.findByRole("dialog", { name: tr("Delete bucket") })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: tr("Cancel") }));

    // A folder row of the tree opens with Enter ("open" in the buckets pane).
    act(() => treeRow("demo-photos")?.focus());
    await user.keyboard("{ArrowRight}");
    await within(screen.getByRole("tree", { name: tr("Buckets and folders") })).findByText("photos");
    act(() => treeRow("photos")?.focus());
    await user.keyboard("{Enter}");
    expect(await location("photos")).toBeInTheDocument();
  });

  it("is covered: every shortcut shown in any menu has a case above", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const shown = new Set<string>();
    const collect = async (pane: "files" | "buckets", name: string) => {
      for (const entry of within(screen.getByRole("menu", { name })).getAllByRole("menuitem")) {
        if (entry.querySelector(".bb-menu-shortcut") !== null) {
          shown.add(`${entry.getAttribute("data-action-id")}@${pane}`);
        }
      }
      await user.keyboard("{Escape}");
    };
    fireEvent.contextMenu(screen.getByRole("grid", { name: tr("Objects") }));
    await collect("files", tr("Objects menu"));
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await collect("files", tr("File menu"));
    fireEvent.contextMenu(await findObjectRow("documents"));
    await collect("files", tr("Folder menu"));
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("documents"));
    await user.keyboard("{/Control}");
    fireEvent.contextMenu(await findObjectRow("documents"));
    await collect("files", tr("Selection menu"));
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await collect("buckets", tr("Bucket menu"));
    fireEvent.contextMenu(document.querySelector(".bb-tree-pane")!);
    await collect("buckets", tr("Buckets menu"));
    const tree = screen.getByRole("tree", { name: tr("Buckets and folders") });
    act(() => within(tree).getByText("demo-photos").closest<HTMLElement>('[role="treeitem"]')?.focus());
    await user.keyboard("{ArrowRight}");
    fireEvent.contextMenu(await within(tree).findByText("photos"));
    await collect("buckets", tr("Folder menu"));
    for (const key of shown) {
      expect(PARITY as readonly string[], `${key} is shown in a menu but has no keyboard case`).toContain(key);
    }
    expect(shown).toEqual(new Set(PARITY));
  });
});
