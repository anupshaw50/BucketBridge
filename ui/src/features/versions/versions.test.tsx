import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { MOCK_HOME } from "../../mock/world";
import { findObjectRow, openBucket, renderApp, statusBar, TestPlatform } from "../../test/renderApp";

function nth<T>(items: readonly T[], index: number): T {
  const item = items[index];
  if (item === undefined) {
    throw new Error(`no item at ${index} of ${items.length}`);
  }
  return item;
}

/** Opens the versions of the object or folder with this name in demo-photos. */
async function openVersions(user: UserEvent, name: string): Promise<HTMLElement> {
  await openBucket(user, "demo-photos");
  await user.click(await findObjectRow(name));
  await user.click(screen.getByRole("button", { name: "Versions" }));
  return screen.findByRole("dialog", { name: "Versions" });
}

const versionRows = (dialog: HTMLElement, key: string) =>
  within(dialog).queryAllByRole("row", { name: new RegExp(`^(Version|Delete marker) .* of ${key.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&")}$`) });

async function apply(user: UserEvent, dialog: HTMLElement) {
  await user.click(within(dialog).getByRole("button", { name: "Apply" }));
}

describe("the versions of an object", () => {
  it("lists them newest first with the current one marked, and opens from the toolbar", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const open = screen.getByRole("button", { name: "Versions" });
    expect(open).toBeEnabled();
    expect(screen.getByRole("button", { name: "Preview" })).toBeDisabled();
    await user.click(await findObjectRow("readme.md"));
    await user.click(open);
    const dialog = await screen.findByRole("dialog", { name: "Versions" });
    expect(within(dialog).getByText("Versions of s3://demo-photos/readme.md")).toBeInTheDocument();
    await within(dialog).findByRole("row", { name: "Version v-readme-2 of readme.md" });
    const rows = versionRows(dialog, "readme.md");
    expect(rows).toHaveLength(3);
    expect(within(nth(rows, 0)).getByText("Latest")).toBeInTheDocument();
    expect(within(nth(rows, 0)).getByText("1.0 KiB")).toBeInTheDocument();
    expect(within(nth(rows, 1)).getByText("v-readme-2")).toBeInTheDocument();
    expect(within(nth(rows, 1)).getByText("900 B")).toBeInTheDocument();
    expect(within(nth(rows, 2)).getByText("v-readme-1")).toBeInTheDocument();
    expect(within(dialog).getByText("1 key, 3 entries shown")).toBeInTheDocument();
    // The current version cannot be restored over itself.
    expect(within(nth(rows, 0)).getByRole("button", { name: /^Restore version/ })).toBeDisabled();
    expect(within(nth(rows, 1)).getByRole("button", { name: "Restore version v-readme-2" })).toBeEnabled();
  });

  it("shows the versions of a folder, with deleted keys and their delete markers", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    expect(within(dialog).getByText("Versions under s3://demo-photos/documents/")).toBeInTheDocument();
    const header = await within(dialog).findByRole("row", { name: "Key documents/old-draft.txt" });
    expect(within(header).getByText("Deleted")).toBeInTheDocument();
    expect(within(header).getByText("3 versions")).toBeInTheDocument();
    const marker = within(dialog).getByRole("row", { name: "Delete marker dm-draft of documents/old-draft.txt" });
    expect(within(marker).getByText("Delete marker")).toBeInTheDocument();
    expect(marker).toHaveAttribute("data-marker", "true");
    // A delete marker has no content: it can only be removed.
    expect(within(marker).getByRole("button", { name: "Remove delete marker dm-draft" })).toBeInTheDocument();
    for (const action of ["Preview", "Download", "Restore"]) {
      expect(within(marker).queryByRole("button", { name: new RegExp(`^${action}`) })).not.toBeInTheDocument();
    }
    expect(within(dialog).getByRole("row", { name: "Key documents/notes.txt" })).toBeInTheDocument();
    expect(within(dialog).getByRole("row", { name: "Key documents/budget 2026.xlsx" })).toBeInTheDocument();
  });

  it("filters: delete markers hidden or only, versions off, several versions, counts and dates", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    await within(dialog).findByRole("row", { name: "Key documents/old-draft.txt" });
    const keysShown = () =>
      within(dialog)
        .queryAllByRole("row", { name: /^Key / })
        .map((row) => row.getAttribute("aria-label")?.replace("Key documents/", ""));

    // Delete markers hidden: the deleted key keeps its two versions.
    await user.click(within(dialog).getByText("Hide"));
    await apply(user, dialog);
    await waitFor(() => expect(within(dialog).queryByRole("row", { name: /Delete marker dm-draft/ })).not.toBeInTheDocument());
    const draft = within(dialog).getByRole("row", { name: "Key documents/old-draft.txt" });
    expect(within(draft).getByText("2 of 3 versions")).toBeInTheDocument();

    // Only delete markers.
    await user.click(within(dialog).getByText("Only"));
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["old-draft.txt"]));
    expect(versionRows(dialog, "documents/old-draft.txt")).toHaveLength(1);

    // Versions off with delete markers hidden would show nothing: refused.
    await user.click(within(dialog).getByText("Hide"));
    await user.click(within(dialog).getByRole("switch", { name: "Show versions" }));
    await apply(user, dialog);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("Nothing would be listed");
    // Versions off, markers included: only the markers.
    await user.click(within(dialog).getByText("Include"));
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["old-draft.txt"]));

    // Back to everything, then keys with several versions.
    await user.click(within(dialog).getByRole("button", { name: "Reset" }));
    await waitFor(() => expect(keysShown()).toHaveLength(4));
    await user.click(within(dialog).getByRole("switch", { name: "Only keys with several versions" }));
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["notes.txt", "old-draft.txt", "report-q3.pdf"]));

    // More than three versions.
    await user.click(within(dialog).getByRole("button", { name: "Reset" }));
    await user.type(within(dialog).getByRole("textbox", { name: /More than/ }), "3");
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["notes.txt"]));
    // Counts nothing can satisfy are explained, not sent.
    await user.type(within(dialog).getByRole("textbox", { name: /Fewer than/ }), "4");
    await apply(user, dialog);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("No key can have at least 4 and fewer than 4 versions");

    // A date range, both days included.
    await user.click(within(dialog).getByRole("button", { name: "Reset" }));
    fireEvent.change(within(dialog).getByLabelText("Modified from"), { target: { value: "2026-10-10" } });
    fireEvent.change(within(dialog).getByLabelText("Modified to"), { target: { value: "2026-10-14" } });
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["notes.txt", "old-draft.txt", "report-q3.pdf"]));
    expect(versionRows(dialog, "documents/notes.txt")).toHaveLength(1);
    fireEvent.change(within(dialog).getByLabelText("Modified to"), { target: { value: "2026-10-01" } });
    await apply(user, dialog);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("The end date is before the start date.");
  });

  it("loads more keys when the page is full", async () => {
    const { user } = await renderApp({ mock: { pageSize: 2 } });
    const dialog = await openVersions(user, "documents");
    await within(dialog).findByRole("row", { name: "Key documents/notes.txt" });
    expect(within(dialog).getAllByRole("row", { name: /^Key / })).toHaveLength(2);
    expect(within(dialog).getByText("2 keys, 5 entries shown")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Load more" }));
    await within(dialog).findByRole("row", { name: "Key documents/report-q3.pdf" });
    expect(within(dialog).getAllByRole("row", { name: /^Key / })).toHaveLength(4);
    expect(within(dialog).queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("restores a version as the new current one and refreshes the list", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "readme.md");
    const row = await within(dialog).findByRole("row", { name: "Version v-readme-2 of readme.md" });
    await user.click(within(row).getByRole("button", { name: "Restore version v-readme-2" }));
    expect(await statusBar().findByText(/^Restored the version of readme\.md from .*; it is now the current version\.$/)).toBeInTheDocument();
    await waitFor(() => expect(versionRows(dialog, "readme.md")).toHaveLength(4));
    const rows = versionRows(dialog, "readme.md");
    expect(within(nth(rows, 0)).getByText("Latest")).toBeInTheDocument();
    expect(within(nth(rows, 0)).getByText("900 B")).toBeInTheDocument();
    expect(within(nth(rows, 1)).queryByText("Latest")).not.toBeInTheDocument();
  });

  it("purges one version only after the word purge is typed", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "readme.md");
    const row = await within(dialog).findByRole("row", { name: "Version v-readme-1 of readme.md" });
    await user.click(within(row).getByRole("button", { name: "Purge version v-readme-1" }));
    const confirm = await screen.findByRole("dialog", { name: "Purge versions" });
    expect(within(confirm).getByText(/Permanently delete version v-readme-1 of "readme\.md"/)).toBeInTheDocument();
    const purge = within(confirm).getByRole("button", { name: "Purge" });
    expect(purge).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "purg");
    expect(purge).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "e");
    expect(purge).toBeEnabled();
    await user.click(purge);
    expect(await statusBar().findByText("Purged 1 version.")).toBeInTheDocument();
    await waitFor(() => expect(versionRows(dialog, "readme.md")).toHaveLength(2));
    expect(within(dialog).queryByText("v-readme-1")).not.toBeInTheDocument();

    // Cancelling purges nothing.
    const other = within(dialog).getByRole("row", { name: "Version v-readme-2 of readme.md" });
    await user.click(within(other).getByRole("button", { name: "Purge version v-readme-2" }));
    const second = await screen.findByRole("dialog", { name: "Purge versions" });
    await user.click(within(second).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Purge versions" })).not.toBeInTheDocument());
    expect(versionRows(dialog, "readme.md")).toHaveLength(2);
  });

  it("purges the selected versions together", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "readme.md");
    await within(dialog).findByRole("row", { name: "Version v-readme-1 of readme.md" });
    const purge = within(dialog).getByRole("button", { name: /^Purge selected/ });
    expect(purge).toBeDisabled();
    await user.click(within(dialog).getByRole("checkbox", { name: "Select version v-readme-1" }));
    await user.click(within(dialog).getByRole("checkbox", { name: "Select version v-readme-2" }));
    expect(purge).toHaveTextContent("Purge selected (2)...");
    await user.click(purge);
    const confirm = await screen.findByRole("dialog", { name: "Purge versions" });
    expect(within(confirm).getByText(/Permanently delete 2 versions \(delete markers included\)/)).toBeInTheDocument();
    await user.type(within(confirm).getByRole("textbox"), "purge");
    await user.click(within(confirm).getByRole("button", { name: "Purge" }));
    expect(await statusBar().findByText("Purged 2 versions.")).toBeInTheDocument();
    await waitFor(() => expect(versionRows(dialog, "readme.md")).toHaveLength(1));

    // Select all shown, then clear it again.
    const all = within(dialog).getByRole("checkbox", { name: "Select all shown" });
    await user.click(all);
    expect(within(dialog).getByRole("button", { name: /^Purge selected/ })).toHaveTextContent("(1)");
    await user.click(all);
    expect(within(dialog).getByRole("button", { name: /^Purge selected/ })).toBeDisabled();
  });

  it("brings a deleted key back by removing its delete marker", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    const marker = await within(dialog).findByRole("row", { name: "Delete marker dm-draft of documents/old-draft.txt" });
    await user.click(within(marker).getByRole("button", { name: "Remove delete marker dm-draft" }));
    const confirm = await screen.findByRole("dialog", { name: "Purge versions" });
    expect(within(confirm).getByText(/the delete marker dm-draft of "documents\/old-draft\.txt"/)).toBeInTheDocument();
    await user.type(within(confirm).getByRole("textbox"), "purge");
    await user.click(within(confirm).getByRole("button", { name: "Purge" }));
    expect(await statusBar().findByText("Purged 1 version.")).toBeInTheDocument();
    await waitFor(() => expect(within(dialog).queryByRole("row", { name: /Delete marker dm-draft/ })).not.toBeInTheDocument());
    const header = within(dialog).getByRole("row", { name: "Key documents/old-draft.txt" });
    expect(within(header).queryByText("Deleted")).not.toBeInTheDocument();
  });

  it("downloads one version to the file the person picks, through the transfer queue", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push(`${MOCK_HOME}/Downloads/readme (older).md`);
    const { user, advance, mock } = await renderApp({ platform });
    const dialog = await openVersions(user, "readme.md");
    const row = await within(dialog).findByRole("row", { name: "Version v-readme-2 of readme.md" });
    await user.click(within(row).getByRole("button", { name: "Download version v-readme-2" }));
    expect(await statusBar().findByText(/^Download of the version from .*: added to the transfer queue\.$/)).toBeInTheDocument();
    expect(platform.saveRequests).toHaveLength(1);
    expect(nth(platform.saveRequests, 0).defaultPath).toMatch(/^readme \(\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}\)\.md$/);
    const list = screen.getByRole("list", { name: "Transfers" });
    expect(within(list).getAllByRole("listitem").some((item) => (item.getAttribute("aria-label") ?? "").includes("readme"))).toBe(true);
    await advance(2_000);
    expect(mock.fs.stat(`${MOCK_HOME}/Downloads/readme (older).md`)?.size).toBe(900);

    // A cancelled picker queues nothing.
    const again = within(dialog).getByRole("row", { name: "Version v-readme-1 of readme.md" });
    await user.click(within(again).getByRole("button", { name: "Download version v-readme-1" }));
    await waitFor(() => expect(platform.saveRequests).toHaveLength(2));
    expect((await mock.list_tasks()).filter((task) => task.kind === "download" && task.dst?.type === "local")).toHaveLength(2);
  });

  it("previews a version from its row, on top of the list", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    const row = await within(dialog).findByRole("row", { name: "Version v-notes-3 of documents/notes.txt" });
    await user.click(within(row).getByRole("button", { name: "Preview version v-notes-3" }));
    const preview = await screen.findByRole("dialog", { name: /Preview: notes\.txt/ });
    expect(within(preview).getByText("Version v-notes-3")).toBeInTheDocument();
    expect(await within(preview).findByLabelText("Contents of notes.txt")).toHaveTextContent("notes-3 line 0000000");
    // The list is still there underneath.
    expect(screen.getByRole("dialog", { name: "Versions" })).toBeInTheDocument();
  });

  it("explains a listing that fails", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    const original = mock.list_versions.bind(mock);
    let fail = true;
    mock.list_versions = (args) =>
      fail ? Promise.reject(new Error("The server did not answer.")) : original(args);
    await user.click(screen.getByRole("button", { name: "Versions" }));
    const dialog = await screen.findByRole("dialog", { name: "Versions" });
    expect(await within(dialog).findByText("The server did not answer.")).toBeInTheDocument();
    fail = false;
    await user.click(within(dialog).getByRole("button", { name: "Try again" }));
    await within(dialog).findByRole("row", { name: "Version v-readme-2 of readme.md" });
  });
});
