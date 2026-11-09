import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { MOCK_HOME } from "../../mock/world";
import { findObjectRow, openBucket, renderApp, statusBar, TestPlatform } from "../../test/renderApp";

/** Escapes a string for use in a RegExp. */
const esc = (text: string): string => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/\n/g, "\\s*");

/** The wildcard slot: pass it as a variable the test does not know. */
const W = "\u0000";

/** Builds a RegExp from a translated sentence whose wildcard variables match anything. */
function sentence(template: string, vars: Record<string, string | number> = {}): RegExp {
  return new RegExp(esc(tr(template, vars)).split(esc(W)).join("[\\s\\S]*?"));
}

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
  await user.click(screen.getByRole("button", { name: tr("Versions") }));
  return screen.findByRole("dialog", { name: tr("Versions") });
}

/** The rows that name a version or delete marker of this key. */
const versionRows = (dialog: HTMLElement, key: string) =>
  within(dialog).queryAllByRole("row", { name: sentence("Version {id} of {key}", { id: W, key }) }).concat(
    within(dialog).queryAllByRole("row", { name: sentence("Delete marker {id} of {key}", { id: W, key }) }),
  );

async function apply(user: UserEvent, dialog: HTMLElement) {
  await user.click(within(dialog).getByRole("button", { name: tr("Apply") }));
}

describe("the versions of an object", () => {
  it("lists them newest first with the current one marked, and opens from the toolbar", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const open = screen.getByRole("button", { name: tr("Versions") });
    expect(open).toBeEnabled();
    expect(screen.getByRole("button", { name: tr("Preview") })).toBeDisabled();
    await user.click(await findObjectRow("readme.md"));
    await user.click(open);
    const dialog = await screen.findByRole("dialog", { name: tr("Versions") });
    expect(within(dialog).getByText(`${tr("Versions of")} s3://demo-photos/readme.md`)).toBeInTheDocument();
    await within(dialog).findByRole(
      "row",
      { name: tr("Version {id} of {key}", { id: "v-readme-2", key: "readme.md" }) },
    );
    const rows = versionRows(dialog, "readme.md");
    expect(rows).toHaveLength(3);
    expect(within(nth(rows, 0)).getByText(tr("Latest"))).toBeInTheDocument();
    expect(within(nth(rows, 0)).getByText("1.0 KiB")).toBeInTheDocument();
    expect(within(nth(rows, 1)).getByText("v-readme-2")).toBeInTheDocument();
    expect(within(nth(rows, 1)).getByText("900 B")).toBeInTheDocument();
    expect(within(nth(rows, 2)).getByText("v-readme-1")).toBeInTheDocument();
    expect(within(dialog).getByText(tr("1 key, {count} entries shown", { count: 3 }))).toBeInTheDocument();
    // The current version cannot be restored over itself.
    expect(within(nth(rows, 0)).getByRole("button", { name: sentence("Restore version {id}", { id: W }) })).toBeDisabled();
    expect(
      within(nth(rows, 1)).getByRole("button", { name: tr("Restore version {id}", { id: "v-readme-2" }) }),
    ).toBeEnabled();
  });

  it("shows the versions of a folder, with deleted keys and their delete markers", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    expect(within(dialog).getByText(`${tr("Versions under")} s3://demo-photos/documents/`)).toBeInTheDocument();
    const header = await within(dialog).findByRole(
      "row",
      { name: tr("Key {key}", { key: "documents/old-draft.txt" }) },
    );
    expect(within(header).getByText(tr("Deleted"))).toBeInTheDocument();
    expect(within(header).getByText(tr("{count} versions", { count: 3 }))).toBeInTheDocument();
    const marker = within(dialog).getByRole(
      "row",
      { name: tr("Delete marker {id} of {key}", { id: "dm-draft", key: "documents/old-draft.txt" }) },
    );
    expect(within(marker).getByText(tr("Delete marker"))).toBeInTheDocument();
    expect(marker).toHaveAttribute("data-marker", "true");
    // A delete marker has no content: it can only be removed.
    expect(within(marker).getByRole("button", { name: tr("Remove delete marker {id}", { id: "dm-draft" }) })).toBeInTheDocument();
    for (const label of ["Preview version {id}", "Download version {id}", "Restore version {id}"]) {
      const prefix = tr(label, { id: "" }).trimEnd();
      expect(within(marker).queryByRole("button", { name: new RegExp(`^${esc(prefix)}`) })).not.toBeInTheDocument();
    }
    expect(within(dialog).getByRole("row", { name: tr("Key {key}", { key: "documents/notes.txt" }) })).toBeInTheDocument();
    expect(within(dialog).getByRole("row", { name: tr("Key {key}", { key: "documents/budget 2026.xlsx" }) })).toBeInTheDocument();
  });

  it("filters: delete markers hidden or only, versions off, several versions, counts and dates", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    await within(dialog).findByRole("row", { name: tr("Key {key}", { key: "documents/old-draft.txt" }) });
    const keysShown = () =>
      within(dialog)
        .queryAllByRole("row", { name: new RegExp(`^${esc(tr("Key {key}", { key: "" }))}`) })
        .map((row) => row.getAttribute("aria-label")?.replace(tr("Key {key}", { key: "documents/" }), ""))
        .map((name) => name?.trim() ?? "");

    // Delete markers hidden: the deleted key keeps its two versions.
    await user.click(within(dialog).getByText(tr("Hide")));
    await apply(user, dialog);
    await waitFor(() =>
      expect(within(dialog).queryByRole("row", { name: sentence("Delete marker {id} of {key}", { id: "dm-draft", key: W }) })).not.toBeInTheDocument(),
    );
    const draft = within(dialog).getByRole("row", { name: tr("Key {key}", { key: "documents/old-draft.txt" }) });
    expect(within(draft).getByText(tr("{matching} of {count} versions", { matching: 2, count: 3 }))).toBeInTheDocument();

    // Only delete markers.
    await user.click(within(dialog).getByText(tr("Only")));
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["old-draft.txt"]));
    expect(versionRows(dialog, "documents/old-draft.txt")).toHaveLength(1);

    // Versions off with delete markers hidden would show nothing: refused.
    await user.click(within(dialog).getByText(tr("Hide")));
    await user.click(within(dialog).getByRole("switch", { name: tr("Show versions") }));
    await apply(user, dialog);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(tr("Nothing would be listed: show versions or delete markers."));
    // Versions off, markers included: only the markers.
    await user.click(within(dialog).getByText(tr("Include")));
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["old-draft.txt"]));

    // Back to everything, then keys with several versions.
    await user.click(within(dialog).getByRole("button", { name: tr("Reset") }));
    await waitFor(() => expect(keysShown()).toHaveLength(4));
    await user.click(within(dialog).getByRole("switch", { name: tr("Only keys with several versions") }));
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["notes.txt", "old-draft.txt", "report-q3.pdf"]));

    // More than three versions.
    await user.click(within(dialog).getByRole("button", { name: tr("Reset") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("More than") }), "3");
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["notes.txt"]));
    // Counts nothing can satisfy are explained, not sent.
    await user.type(within(dialog).getByRole("textbox", { name: tr("Fewer than") }), "4");
    await apply(user, dialog);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      tr("No key can have at least {least} and fewer than {fewer} versions.", { least: 4, fewer: 4 }),
    );

    // A date range, both days included.
    await user.click(within(dialog).getByRole("button", { name: tr("Reset") }));
    fireEvent.change(within(dialog).getByLabelText(tr("Modified from")), { target: { value: "2026-10-10" } });
    fireEvent.change(within(dialog).getByLabelText(tr("Modified to")), { target: { value: "2026-10-14" } });
    await apply(user, dialog);
    await waitFor(() => expect(keysShown()).toEqual(["notes.txt", "old-draft.txt", "report-q3.pdf"]));
    expect(versionRows(dialog, "documents/notes.txt")).toHaveLength(1);
    fireEvent.change(within(dialog).getByLabelText(tr("Modified to")), { target: { value: "2026-10-01" } });
    await apply(user, dialog);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(tr("The end date is before the start date."));
  });

  it("loads more keys when the page is full", async () => {
    const { user } = await renderApp({ mock: { pageSize: 2 } });
    const dialog = await openVersions(user, "documents");
    await within(dialog).findByRole("row", { name: tr("Key {key}", { key: "documents/notes.txt" }) });
    expect(within(dialog).getAllByRole("row", { name: new RegExp(`^${esc(tr("Key {key}", { key: "" }))}`) })).toHaveLength(2);
    expect(within(dialog).getByText(tr("{keys} keys, {count} entries shown", { keys: 2, count: 5 }))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Load more") }));
    await within(dialog).findByRole("row", { name: tr("Key {key}", { key: "documents/report-q3.pdf" }) });
    expect(within(dialog).getAllByRole("row", { name: new RegExp(`^${esc(tr("Key {key}", { key: "" }))}`) })).toHaveLength(4);
    expect(within(dialog).queryByRole("button", { name: tr("Load more") })).not.toBeInTheDocument();
  });

  it("restores a version as the new current one and refreshes the list", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "readme.md");
    const row = await within(dialog).findByRole(
      "row",
      { name: tr("Version {id} of {key}", { id: "v-readme-2", key: "readme.md" }) },
    );
    await user.click(within(row).getByRole("button", { name: tr("Restore version {id}", { id: "v-readme-2" }) }));
    await statusBar().findByText(
      sentence("Restored the version of {key} from {time}; it is now the current version.", { key: "readme.md", time: W }),
    );
    await waitFor(() => expect(versionRows(dialog, "readme.md")).toHaveLength(4));
    const rows = versionRows(dialog, "readme.md");
    expect(within(nth(rows, 0)).getByText(tr("Latest"))).toBeInTheDocument();
    expect(within(nth(rows, 0)).getByText("900 B")).toBeInTheDocument();
    expect(within(nth(rows, 1)).queryByText(tr("Latest"))).not.toBeInTheDocument();
  });

  it("purges one version only after the word purge is typed", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "readme.md");
    const row = await within(dialog).findByRole("row", { name: tr("Version {id} of {key}", { id: "v-readme-1", key: "readme.md" }) });
    await user.click(within(row).getByRole("button", { name: tr("Purge version {id}", { id: "v-readme-1" }) }));
    const confirm = await screen.findByRole("dialog", { name: tr("Purge versions") });
    expect(
      within(confirm).getByText(
        sentence("Permanently delete {what} from {bucket}?\nPurged versions cannot be recovered.", {
          what: tr("version {id} of \"{key}\" ({modified}, {size})", { id: "v-readme-1", key: "readme.md", modified: W, size: W }),
          bucket: "demo-photos",
        }),
      ),
    ).toBeInTheDocument();
    const purge = within(confirm).getByRole("button", { name: tr("Purge") });
    expect(purge).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "purg");
    expect(purge).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "e");
    expect(purge).toBeEnabled();
    await user.click(purge);
    expect(await statusBar().findByText(tr("Purged 1 version."))).toBeInTheDocument();
    await waitFor(() => expect(versionRows(dialog, "readme.md")).toHaveLength(2));
    expect(within(dialog).queryByText("v-readme-1")).not.toBeInTheDocument();

    // Cancelling purges nothing.
    const other = within(dialog).getByRole("row", { name: tr("Version {id} of {key}", { id: "v-readme-2", key: "readme.md" }) });
    await user.click(within(other).getByRole("button", { name: tr("Purge version {id}", { id: "v-readme-2" }) }));
    const second = await screen.findByRole("dialog", { name: tr("Purge versions") });
    await user.click(within(second).getByRole("button", { name: tr("Cancel") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Purge versions") })).not.toBeInTheDocument());
    expect(versionRows(dialog, "readme.md")).toHaveLength(2);
  });

  it("purges the selected versions together", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "readme.md");
    await within(dialog).findByRole("row", { name: tr("Version {id} of {key}", { id: "v-readme-1", key: "readme.md" }) });
    const purge = within(dialog).getByRole("button", { name: sentence("Purge selected ({count})...", { count: W }) });
    expect(purge).toBeDisabled();
    await user.click(within(dialog).getByRole("checkbox", { name: tr("Select version {id}", { id: "v-readme-1" }) }));
    await user.click(within(dialog).getByRole("checkbox", { name: tr("Select version {id}", { id: "v-readme-2" }) }));
    expect(purge).toHaveTextContent(tr("Purge selected ({count})...", { count: 2 }));
    await user.click(purge);
    const confirm = await screen.findByRole("dialog", { name: tr("Purge versions") });
    expect(
      within(confirm).getByText(
        sentence("Permanently delete {what} from {bucket}?\nPurged versions cannot be recovered.", {
          what: tr("{count} versions (delete markers included)", { count: 2 }),
          bucket: "demo-photos",
        }),
      ),
    ).toBeInTheDocument();
    await user.type(within(confirm).getByRole("textbox"), "purge");
    await user.click(within(confirm).getByRole("button", { name: tr("Purge") }));
    expect(await statusBar().findByText(tr("Purged {count} versions.", { count: 2 }))).toBeInTheDocument();
    await waitFor(() => expect(versionRows(dialog, "readme.md")).toHaveLength(1));

    // Select all shown, then clear it again.
    const all = within(dialog).getByRole("checkbox", { name: tr("Select all shown") });
    await user.click(all);
    expect(within(dialog).getByRole("button", { name: sentence("Purge selected ({count})...", { count: W }) })).toHaveTextContent(
      tr("Purge selected ({count})...", { count: 1 }),
    );
    await user.click(all);
    expect(within(dialog).getByRole("button", { name: sentence("Purge selected ({count})...", { count: W }) })).toBeDisabled();
  });

  it("brings a deleted key back by removing its delete marker", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    const marker = await within(dialog).findByRole(
      "row",
      { name: tr("Delete marker {id} of {key}", { id: "dm-draft", key: "documents/old-draft.txt" }) },
    );
    await user.click(within(marker).getByRole("button", { name: tr("Remove delete marker {id}", { id: "dm-draft" }) }));
    const confirm = await screen.findByRole("dialog", { name: tr("Purge versions") });
    expect(
      within(confirm).getByText(
        sentence("Permanently delete {what} from {bucket}?\nPurged versions cannot be recovered.", {
          what: tr("the delete marker {id} of \"{key}\"", { id: "dm-draft", key: "documents/old-draft.txt" }),
          bucket: "demo-photos",
        }),
      ),
    ).toBeInTheDocument();
    await user.type(within(confirm).getByRole("textbox"), "purge");
    await user.click(within(confirm).getByRole("button", { name: tr("Purge") }));
    expect(await statusBar().findByText(tr("Purged 1 version."))).toBeInTheDocument();
    await waitFor(() =>
      expect(within(dialog).queryByRole("row", { name: sentence("Delete marker {id} of {key}", { id: "dm-draft", key: W }) })).not.toBeInTheDocument(),
    );
    const header = within(dialog).getByRole("row", { name: tr("Key {key}", { key: "documents/old-draft.txt" }) });
    expect(within(header).queryByText(tr("Deleted"))).not.toBeInTheDocument();
  });

  it("downloads one version to the file the person picks, through the transfer queue", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push(`${MOCK_HOME}/Downloads/readme (older).md`);
    const { user, advance, mock } = await renderApp({ platform });
    const dialog = await openVersions(user, "readme.md");
    const row = await within(dialog).findByRole("row", { name: tr("Version {id} of {key}", { id: "v-readme-2", key: "readme.md" }) });
    await user.click(within(row).getByRole("button", { name: tr("Download version {id}", { id: "v-readme-2" }) }));
    await statusBar().findByText(
      sentence("Download of the version from {time}: added to the transfer queue.", { time: W }),
    );
    expect(platform.saveRequests).toHaveLength(1);
    expect(nth(platform.saveRequests, 0).defaultPath).toMatch(/^readme \(\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}\)\.md$/);
    const listEl = screen.getByRole("list", { name: tr("Transfers") });
    expect(
      Array.from(listEl.querySelectorAll("[role=listitem]")).some((item) => (item.getAttribute("aria-label") ?? "").includes("readme")),
    ).toBe(true);
    await advance(2_000);
    expect(mock.fs.stat(`${MOCK_HOME}/Downloads/readme (older).md`)?.size).toBe(900);

    // A cancelled picker queues nothing.
    const again = within(dialog).getByRole("row", { name: tr("Version {id} of {key}", { id: "v-readme-1", key: "readme.md" }) });
    await user.click(within(again).getByRole("button", { name: tr("Download version {id}", { id: "v-readme-1" }) }));
    await waitFor(() => expect(platform.saveRequests).toHaveLength(2));
    expect((await mock.list_tasks()).filter((task) => task.kind === "download" && task.dst?.type === "local")).toHaveLength(2);
  });

  it("previews a version from its row, on top of the list", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    const row = await within(dialog).findByRole("row", { name: tr("Version {id} of {key}", { id: "v-notes-3", key: "documents/notes.txt" }) });
    await user.click(within(row).getByRole("button", { name: tr("Preview version {id}", { id: "v-notes-3" }) }));
    const preview = await screen.findByRole("dialog", { name: new RegExp(esc(tr("Preview: {name}", { name: "notes.txt" }))) });
    expect(within(preview).getByText(tr("Version {id}", { id: "v-notes-3" }))).toBeInTheDocument();
    expect(await within(preview).findByLabelText(tr("Contents of {name}", { name: "notes.txt" }))).toHaveTextContent("notes-3 line 0000000");
    // The list is still there underneath.
    expect(screen.getByRole("dialog", { name: tr("Versions") })).toBeInTheDocument();
  });

  it("explains a listing that fails", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    const original = mock.list_versions.bind(mock);
    let fail = true;
    mock.list_versions = (args) =>
      fail ? Promise.reject(new Error("The server did not answer.")) : original(args);
    await user.click(screen.getByRole("button", { name: tr("Versions") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Versions") });
    expect(await within(dialog).findByText("The server did not answer.")).toBeInTheDocument();
    fail = false;
    await user.click(within(dialog).getByRole("button", { name: tr("Try again") }));
    await within(dialog).findByRole("row", { name: tr("Version {id} of {key}", { id: "v-readme-2", key: "readme.md" }) });
  });
});

describe("flat view, sorting and bulk selection", () => {
  const rowKeys = (dialog: HTMLElement): string[] =>
    within(dialog)
      .getAllByRole("row")
      .filter((row) => row.classList.contains("bb-version-row"))
      .map((row) => row.getAttribute("aria-label") ?? "");

  it("shows one list without group rows, with a key column, and goes back to groups", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    await within(dialog).findByRole("row", { name: tr("Key {key}", { key: "documents/old-draft.txt" }) });
    expect(within(dialog).queryByRole("columnheader", { name: tr("Sort by {column}", { column: tr("Key") }) })).toBeNull();
    await user.click(within(dialog).getByText(tr("Flat list")));
    expect(within(dialog).queryByRole("row", { name: tr("Key {key}", { key: "documents/old-draft.txt" }) })).toBeNull();
    expect(within(dialog).getByRole("button", { name: tr("Sort by {column}", { column: tr("Key") }) })).toBeInTheDocument();
    const flat = rowKeys(dialog);
    expect(flat.some((label) => label.includes("documents/notes.txt"))).toBe(true);
    expect(flat.some((label) => label.includes("documents/old-draft.txt"))).toBe(true);
    await user.click(within(dialog).getByText(tr("Grouped by key")));
    expect(within(dialog).getByRole("row", { name: tr("Key {key}", { key: "documents/old-draft.txt" }) })).toBeInTheDocument();
  });

  it("sorts by a column and returns to the original order on the third click", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    await within(dialog).findByRole("row", { name: tr("Key {key}", { key: "documents/old-draft.txt" }) });
    await user.click(within(dialog).getByText(tr("Flat list")));
    const original = rowKeys(dialog);
    const button = () => within(dialog).getByRole("button", { name: tr("Sort by {column}", { column: tr("Key") }) });
    await user.click(button());
    const ascending = rowKeys(dialog);
    expect(button().closest("th")).toHaveAttribute("aria-sort", "ascending");
    const keyOf = (label: string) => /documents\/.+?\.(?:xlsx|txt|pdf)/.exec(label)?.[0] ?? label;
    expect(ascending.map(keyOf)).toEqual([...ascending.map(keyOf)].sort((a, b) => a.localeCompare(b, undefined, { numeric: true })));
    await user.click(button());
    expect(button().closest("th")).toHaveAttribute("aria-sort", "descending");
    expect(rowKeys(dialog).map(keyOf)).toEqual([...ascending.map(keyOf)].reverse().sort((a, b) => b.localeCompare(a, undefined, { numeric: true })));
    await user.click(button());
    expect(button().closest("th")).toHaveAttribute("aria-sort", "none");
    expect(rowKeys(dialog)).toEqual(original);
  });

  it("selects the newest delete markers and all non-current versions", async () => {
    const { user } = await renderApp();
    const dialog = await openVersions(user, "documents");
    await within(dialog).findByRole("row", { name: tr("Key {key}", { key: "documents/old-draft.txt" }) });
    const checked = () =>
      within(dialog)
        .getAllByRole("checkbox")
        .filter((box) => (box as HTMLInputElement).checked && box.getAttribute("aria-label") !== tr("Select all shown"))
        .map((box) => box.getAttribute("aria-label"));

    await user.click(within(dialog).getByRole("button", { name: tr("Select newest delete markers") }));
    expect(checked()).toEqual([tr("Select delete marker {id}", { id: "dm-draft" })]);
    expect(within(dialog).getByRole("button", { name: sentence("Purge selected ({count})...", { count: 1 }) })).toBeEnabled();

    await user.click(within(dialog).getByRole("button", { name: tr("Clear selection") }));
    expect(checked()).toEqual([]);

    await user.click(within(dialog).getByRole("button", { name: tr("Select all non-current versions") }));
    const nonCurrent = checked();
    expect(nonCurrent.length).toBeGreaterThan(0);
    // The newest entry of every key stays unselected, so the current versions are safe.
    const latest = within(dialog)
      .getAllByRole("row")
      .filter((row) => row.classList.contains("bb-version-row") && within(row).queryByText(tr("Latest")) !== null);
    expect(latest.length).toBeGreaterThan(0);
    for (const row of latest) {
      expect(within(row).getByRole<HTMLInputElement>("checkbox").checked).toBe(false);
    }
    expect(nonCurrent).not.toContain(tr("Select delete marker {id}", { id: "dm-draft" }));
  });
});
