import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";

function item(name: string): HTMLElement {
  return screen.getByRole("menuitem", { name });
}

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
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
      [tr("Versioning…"), tr("Versioning")],
      [tr("Server access logging…"), tr("Logging")],
      [tr("Bucket policy…"), tr("Policy")],
      [tr("Public access block…"), tr("Public access")],
      [tr("Static website…"), tr("Website")],
      [tr("Website routing rules…"), tr("Website")],
      [tr("Transfer acceleration…"), tr("Acceleration")],
    ];
    for (const [entry, tab] of expected) {
      fireEvent.contextMenu(await bucketRow("demo-photos"));
      await chooseFromSubmenu(user, tr("Properties"), entry);
      const dialog = await screen.findByRole("dialog", { name: tr("Bucket properties: {bucket}", { bucket: "demo-photos" }) });
      expect(selectedTab(dialog), entry).toBe(tab);
      await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    }
  });

  it("keeps the features that are not built (ownership, object lock) out of the submenu", async () => {
    const { user } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(item(tr("Properties")));
    const labels = within(screen.getByRole("menu", { name: tr("Properties") }))
      .getAllByRole("menuitem")
      .map((entry) => entry.getAttribute("data-label"));
    expect(labels).toEqual([
      tr("Versioning…"),
      tr("Server access logging…"),
      tr("Bucket policy…"),
      tr("Public access block…"),
      tr("CORS…"),
      tr("Lifecycle rules…"),
      tr("Intelligent-Tiering…"),
      tr("Replication…"),
      tr("Static website…"),
      tr("Website routing rules…"),
      tr("Transfer acceleration…"),
      tr("Object ownership…"),
      tr("Object lock…"),
    ]);
  });

  it("adds an external bucket from the empty area of the buckets pane, and removes it from its own menu", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    const pane = document.querySelector(".bb-tree-pane");
    expect(pane).not.toBeNull();
    fireEvent.contextMenu(pane!);
    await user.click(item(tr("Add external bucket…")));
    const dialog = await screen.findByRole("dialog", { name: tr("Add external bucket") });
    await user.type(within(dialog).getByRole("textbox", { name: new RegExp(tr("Bucket name")) }), "partner-exchange");
    await user.click(within(dialog).getByRole("button", { name: tr("Add bucket") }));
    const id = (await mock.list_accounts()).default_account_id ?? "";
    await waitFor(async () => expect(await mock.list_external_buckets({ account_id: id })).toEqual(["partner-exchange"]));

    fireEvent.contextMenu(await bucketRow("partner-exchange"));
    await user.click(item(tr("Remove external bucket…")));
    await user.click(
      within(await screen.findByRole("dialog", { name: tr("Remove external bucket") })).getByRole("button", { name: tr("Remove") }),
    );
    await waitFor(async () => expect(await mock.list_external_buckets({ account_id: id })).toEqual([]));
    // A bucket of the account itself has no such entry.
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    expect(screen.queryByRole("menuitem", { name: tr("Remove external bucket…") })).toBeNull();
  });
});

describe("tools in the buckets menu and the account menu (T12)", () => {
  it("opens diagnostics and the uncompleted multipart uploads from the empty area of the buckets pane", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    const pane = document.querySelector(".bb-tree-pane");
    fireEvent.contextMenu(pane!);
    await user.click(item(tr("Diagnostics…")));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Diagnostics") })).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    fireEvent.contextMenu(pane!);
    await user.click(item(tr("Uncompleted multipart uploads…")));
    expect(await screen.findByRole("dialog", { name: tr("Uncompleted multipart uploads") })).toBeInTheDocument();
  });

  it("tests the connection of the account that was clicked", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Accounts") }));
    const manager = await screen.findByRole("dialog", { name: tr("Accounts") });
    fireEvent.contextMenu(within(manager).getByText("Secure (MFA)"));
    await user.click(item(tr("Test connection")));
    const dialog = await screen.findByRole("dialog", { name: tr("Diagnostics") });
    expect(within(dialog).getByRole("combobox", { name: tr("Account") })).toHaveValue("Secure (MFA)");
  });
});

describe("object entries of the file and selection menus (T06, T08, T12)", () => {
  it("opens the properties editors on the tab of each metadata entry, and the full sheet from Properties", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const expected: [string, string][] = [
      [tr("Tags…"), tr("Tags")],
      [tr("Storage class…"), tr("Storage class")],
      [tr("Server-side encryption…"), tr("Encryption")],
      [tr("HTTP headers…"), tr("HTTP headers")],
    ];
    for (const [entry, tab] of expected) {
      fireEvent.contextMenu(await findObjectRow("readme.md"));
      await chooseFromSubmenu(user, tr("Metadata"), entry);
      const dialog = await screen.findByRole("dialog", { name: tr("Properties") });
      await waitFor(() => expect(selectedTab(dialog), entry).toBe(tab));
      await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    }
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("ACL…")));
    const acl = await screen.findByRole("dialog", { name: tr("Properties") });
    await waitFor(() => expect(selectedTab(acl)).toBe(tr("Permissions")));
    await user.click(within(acl).getByRole("button", { name: tr("Close") }));
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Properties")));
    const sheet = await screen.findByRole("dialog", { name: tr("Properties") });
    expect(selectedTab(sheet)).toBe(tr("General"));
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
    await user.click(item(tr("Metadata")));
    expect(
      within(screen.getByRole("menu", { name: tr("Metadata") }))
        .getAllByRole("menuitem")
        .map((entry) => entry.getAttribute("data-label")),
    ).toEqual([tr("Storage class…"), tr("Tags…")]);
    await user.click(within(screen.getByRole("menu", { name: tr("Metadata") })).getByRole("menuitem", { name: tr("Tags…") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Properties of {n} objects", { n: 2 }) });
    await waitFor(() => expect(selectedTab(dialog)).toBe(tr("Tags")));
  });

  it("previews, lists the versions and exports from the file menu", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Preview")));
    const preview = await screen.findByRole("dialog", { name: new RegExp(tr("Preview: {name}", { name: "readme.md" })) });
    await user.click(within(preview).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Versions…")));
    const versions = await screen.findByRole("dialog", { name: tr("Versions") });
    expect(within(versions).getByText(`${tr("Versions of")} s3://demo-photos/readme.md`)).toBeInTheDocument();
    await user.click(within(versions).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(item(tr("Export…")));
    expect(await screen.findByRole("dialog", { name: tr("Export listing") })).toBeInTheDocument();
  });

  it("shows the versions of a folder, and opens the folder before the saved filters", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("documents"));
    await user.click(item(tr("Versions…")));
    const versions = await screen.findByRole("dialog", { name: tr("Versions") });
    expect(within(versions).getByText(`${tr("Versions under")} s3://demo-photos/documents/`)).toBeInTheDocument();
    await user.click(within(versions).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    fireEvent.contextMenu(await findObjectRow("documents"));
    await user.click(item(tr("Filter…")));
    expect(await screen.findByRole("dialog", { name: tr("Saved filters") })).toBeInTheDocument();
    await findObjectRow("notes.txt");
  });

  it("offers the saved filters on the empty area of the object list", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(screen.getByRole("grid", { name: tr("Objects") }));
    await user.click(item(tr("Filter…")));
    expect(await screen.findByRole("dialog", { name: tr("Saved filters") })).toBeInTheDocument();
  });
});

describe("the toolbar and the menus share the registry", () => {
  it("shows the shortcut of Preview on its button and runs it through the same entry as F3", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    const button = screen.getByRole("button", { name: tr("Preview") });
    expect(button).toBeEnabled();
    await user.hover(button);
    expect(await screen.findByRole("tooltip")).toHaveTextContent(`${tr("Preview")} (F3)`);
    await user.click(button);
    expect(await screen.findByRole("dialog", { name: new RegExp(tr("Preview: {name}", { name: "readme.md" })) })).toBeInTheDocument();
  });

  it("opens the tools from the Tools menu through the registry", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Tools") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Diagnostics...") }));
    expect(await screen.findByRole("dialog", { name: tr("Diagnostics") })).toBeInTheDocument();
  });
});
