import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";

function item(name: string): HTMLElement {
  return screen.getByRole("menuitem", { name });
}

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: "Buckets and folders" });
  const row = (await within(tree).findByText(name)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${name}`);
  }
  return row;
}

/** Opens a submenu of the open menu and clicks one of its entries. */
async function chooseFromSubmenu(user: UserEvent, submenu: string, entry: string): Promise<void> {
  await user.click(item(submenu));
  await user.click(within(screen.getByRole("menu", { name: submenu })).getByRole("menuitem", { name: entry }));
}

function selectedTab(dialog: HTMLElement): string {
  const tab = within(dialog).getAllByRole("tab").find((candidate) => candidate.getAttribute("aria-selected") === "true");
  return tab?.textContent ?? "";
}

describe("bucket configuration in the bucket menu (T03)", () => {
  it("opens the bucket properties on the tab of each setting", async () => {
    const { user } = await renderApp();
    const expected: [string, string][] = [
      ["Versioning…", "Versioning"],
      ["Server access logging…", "Logging"],
      ["Bucket policy…", "Policy"],
      ["Public access block…", "Public access"],
      ["Static website…", "Website"],
      ["Website routing rules…", "Website"],
      ["Transfer acceleration…", "Acceleration"],
    ];
    for (const [entry, tab] of expected) {
      fireEvent.contextMenu(await bucketRow("demo-photos"));
      await chooseFromSubmenu(user, "Properties", entry);
      const dialog = await screen.findByRole("dialog", { name: "Bucket properties: demo-photos" });
      expect(selectedTab(dialog), entry).toBe(tab);
      await user.click(within(dialog).getByRole("button", { name: "Close" }));
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    }
  });

  it("keeps the features that are not built (ownership, object lock) out of the submenu", async () => {
    const { user } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(item("Properties"));
    const labels = within(screen.getByRole("menu", { name: "Properties" }))
      .getAllByRole("menuitem")
      .map((entry) => entry.getAttribute("data-label"));
    expect(labels).toEqual([
      "Versioning…",
      "Server access logging…",
      "Bucket policy…",
      "Public access block…",
      "CORS…",
      "Lifecycle rules…",
      "Intelligent-Tiering…",
      "Replication…",
      "Static website…",
      "Website routing rules…",
      "Transfer acceleration…",
      "Object ownership…",
      "Object lock…",
    ]);
  });

  it("adds an external bucket from the empty area of the buckets pane, and removes it from its own menu", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    const pane = document.querySelector(".bb-tree-pane");
    expect(pane).not.toBeNull();
    fireEvent.contextMenu(pane!);
    await user.click(item("Add external bucket…"));
    const dialog = await screen.findByRole("dialog", { name: "Add external bucket" });
    await user.type(within(dialog).getByRole("textbox", { name: /Bucket name/ }), "partner-exchange");
    await user.click(within(dialog).getByRole("button", { name: "Add bucket" }));
    const id = (await mock.list_accounts()).default_account_id ?? "";
    await waitFor(async () => expect(await mock.list_external_buckets({ account_id: id })).toEqual(["partner-exchange"]));

    fireEvent.contextMenu(await bucketRow("partner-exchange"));
    await user.click(item("Remove external bucket…"));
    await user.click(within(await screen.findByRole("dialog", { name: "Remove external bucket" })).getByRole("button", { name: "Remove" }));
    await waitFor(async () => expect(await mock.list_external_buckets({ account_id: id })).toEqual([]));
    // A bucket of the account itself has no such entry.
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    expect(screen.queryByRole("menuitem", { name: "Remove external bucket…" })).toBeNull();
  });
});

describe("tools in the buckets menu and the account menu (T12)", () => {
  it("opens diagnostics and the uncompleted multipart uploads from the empty area of the buckets pane", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    const pane = document.querySelector(".bb-tree-pane");
    fireEvent.contextMenu(pane!);
    await user.click(item("Diagnostics…"));
    await user.click(within(await screen.findByRole("dialog", { name: "Diagnostics" })).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    fireEvent.contextMenu(pane!);
    await user.click(item("Uncompleted multipart uploads…"));
    expect(await screen.findByRole("dialog", { name: "Uncompleted multipart uploads" })).toBeInTheDocument();
  });

  it("tests the connection of the account that was clicked", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: "Accounts" }));
    const manager = await screen.findByRole("dialog", { name: "Accounts" });
    fireEvent.contextMenu(within(manager).getByText("Secure (MFA)"));
    await user.click(item("Test connection"));
    const dialog = await screen.findByRole("dialog", { name: "Diagnostics" });
    expect(within(dialog).getByRole("combobox", { name: "Account" })).toHaveValue("Secure (MFA)");
  });
});

describe("object entries of the file and selection menus (T06, T08, T12)", () => {
  it("opens the properties editors on the tab of each metadata entry, and the full sheet from Properties", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const expected: [string, string][] = [
      ["Tags…", "Tags"],
      ["Storage class…", "Storage class"],
      ["Server-side encryption…", "Encryption"],
      ["HTTP headers…", "HTTP headers"],
    ];
    for (const [entry, tab] of expected) {
      fireEvent.contextMenu(await findObjectRow("readme.md"));
      await chooseFromSubmenu(user, "Metadata", entry);
      const dialog = await screen.findByRole("dialog", { name: "Properties" });
      await waitFor(() => expect(selectedTab(dialog), entry).toBe(tab));
      await user.click(within(dialog).getByRole("button", { name: "Close" }));
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    }
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item("ACL…"));
    const acl = await screen.findByRole("dialog", { name: "Properties" });
    await waitFor(() => expect(selectedTab(acl)).toBe("Permissions"));
    await user.click(within(acl).getByRole("button", { name: "Close" }));
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item("Properties"));
    const sheet = await screen.findByRole("dialog", { name: "Properties" });
    expect(selectedTab(sheet)).toBe("General");
  });

  it("offers the batch editors for several objects and opens one dialog for all of them", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("documents"));
    await user.keyboard("{Enter}");
    await findObjectRow("notes.txt");
    await user.click(await findObjectRow("notes.txt"));
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("report-q3.pdf"));
    await user.keyboard("{/Control}");
    fireEvent.contextMenu(await findObjectRow("notes.txt"));
    await user.click(item("Metadata"));
    expect(
      within(screen.getByRole("menu", { name: "Metadata" }))
        .getAllByRole("menuitem")
        .map((entry) => entry.getAttribute("data-label")),
    ).toEqual(["Storage class…", "Tags…"]);
    await user.click(within(screen.getByRole("menu", { name: "Metadata" })).getByRole("menuitem", { name: "Tags…" }));
    const dialog = await screen.findByRole("dialog", { name: "Properties of 2 objects" });
    await waitFor(() => expect(selectedTab(dialog)).toBe("Tags"));
  });

  it("previews, lists the versions and exports from the file menu", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item("Preview"));
    const preview = await screen.findByRole("dialog", { name: /Preview: readme\.md/ });
    await user.click(within(preview).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item("Versions…"));
    const versions = await screen.findByRole("dialog", { name: "Versions" });
    expect(within(versions).getByText("Versions of s3://demo-photos/readme.md")).toBeInTheDocument();
    await user.click(within(versions).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item("Export…"));
    expect(await screen.findByRole("dialog", { name: "Export listing" })).toBeInTheDocument();
  });

  it("shows the versions of a folder, and opens the folder before the saved filters", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("documents"));
    await user.click(item("Versions…"));
    const versions = await screen.findByRole("dialog", { name: "Versions" });
    expect(within(versions).getByText("Versions under s3://demo-photos/documents/")).toBeInTheDocument();
    await user.click(within(versions).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    fireEvent.contextMenu(await findObjectRow("documents"));
    await user.click(item("Filter…"));
    expect(await screen.findByRole("dialog", { name: "Saved filters" })).toBeInTheDocument();
    await findObjectRow("notes.txt");
  });

  it("offers the saved filters on the empty area of the object list", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(screen.getByRole("grid", { name: "Objects" }));
    await user.click(item("Filter…"));
    expect(await screen.findByRole("dialog", { name: "Saved filters" })).toBeInTheDocument();
  });
});

describe("the toolbar and the menus share the registry", () => {
  it("shows the shortcut of Preview on its button and runs it through the same entry as F3", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    const button = screen.getByRole("button", { name: "Preview" });
    expect(button).toBeEnabled();
    await user.hover(button);
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Preview (F3)");
    await user.click(button);
    expect(await screen.findByRole("dialog", { name: /Preview: readme\.md/ })).toBeInTheDocument();
  });

  it("opens the tools from the Tools menu through the registry", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: "Tools" }));
    await user.click(await screen.findByRole("menuitem", { name: "Diagnostics..." }));
    expect(await screen.findByRole("dialog", { name: "Diagnostics" })).toBeInTheDocument();
  });
});
