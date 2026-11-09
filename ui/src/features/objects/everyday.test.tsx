import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import type { MockBackend } from "../../mock/mockBackend";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";

async function storedSettings(mock: MockBackend, change: (draft: Awaited<ReturnType<MockBackend["get_settings"]>>) => void) {
  const settings = await mock.get_settings();
  change(settings);
  await mock.save_settings({ settings });
}

function treeNames(): string[] {
  const tree = screen.getByRole("tree", { name: tr("Buckets and folders") });
  return within(tree)
    .getAllByRole("treeitem")
    .map((row) => row.querySelector(".bb-ellipsis")?.textContent ?? "");
}

describe("address bar", () => {
  it("opens a pasted s3:// path and a plain bucket/prefix path", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    // With no bucket open the field is already there.
    const field = await screen.findByRole("textbox", { name: tr("Path") });
    await user.click(field);
    await user.paste("s3://demo-photos/photos/2024/");
    await user.keyboard("{Enter}");
    await findObjectRow("beach.jpg");

    const location = screen.getByRole("navigation", { name: tr("Location") });
    expect(within(location).getByRole("button", { name: "2024" })).toHaveAttribute("aria-current", "location");

    await user.click(screen.getByRole("button", { name: tr("Edit path") }));
    const edit = screen.getByRole("textbox", { name: tr("Path") });
    expect(edit).toHaveValue("s3://demo-photos/photos/2024/");
    await user.clear(edit);
    await user.type(edit, "demo-photos/documents{Enter}");
    await findObjectRow("notes.txt");
    expect(screen.queryByRole("textbox", { name: tr("Path") })).toBeNull();
  });

  it("explains an invalid path and keeps the field open", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    const field = await screen.findByRole("textbox", { name: tr("Path") });
    await user.type(field, "https://example.com/a{Enter}");
    expect(await screen.findByText(tr("Only s3:// paths can be opened here."))).toBeInTheDocument();
    expect(field).toHaveAttribute("aria-invalid", "true");
    // Typing again clears the message.
    await user.type(field, "x");
    expect(screen.queryByText(tr("Only s3:// paths can be opened here."))).toBeNull();
  });

  it("says when the account cannot see the bucket, and opens one that is only shared with it", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    const field = await screen.findByRole("textbox", { name: tr("Path") });
    await user.type(field, "s3://no-such-bucket/{Enter}");
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(tr("The bucket \"{bucket}\" is not available to this account: {reason}", { bucket: "no-such-bucket", reason: "" }).split(":")[0]);
    expect(alert.textContent).toContain("NoSuchBucket");
    expect(screen.queryByRole("navigation", { name: tr("Location") })).toBeNull();

    // partner-exchange is not in the bucket list but is shared with the demo user.
    await user.clear(field);
    await user.type(field, "s3://partner-exchange/{Enter}");
    await waitFor(() => expect(screen.getByRole("navigation", { name: tr("Location") })).toBeInTheDocument());
    expect(within(screen.getByRole("navigation", { name: tr("Location") })).getByRole("button", { name: "partner-exchange" })).toBeInTheDocument();
  });
});

describe("sort order and column widths", () => {
  it("remembers the sort order in the settings and restores it at start", async () => {
    const { user, mock, unmount } = await renderApp();
    await openBucket(user, "demo-photos");
    await findObjectRow("readme.md");
    await user.click(screen.getByRole("button", { name: tr("Sort by size") }));
    await waitFor(async () => expect((await mock.get_settings()).interface).toMatchObject({ sort_column: "size", sort_direction: "asc" }));
    await user.click(screen.getByRole("button", { name: tr("Sort by size") }));
    await waitFor(async () => expect((await mock.get_settings()).interface).toMatchObject({ sort_column: "size", sort_direction: "desc" }));

    unmount();
    const restored = await renderApp({
      prepare: (backend) => storedSettings(backend, (draft) => {
        draft.interface.sort_column = "modified";
        draft.interface.sort_direction = "desc";
      }),
    });
    await openBucket(restored.user, "demo-photos");
    const headers = screen.getAllByRole("columnheader");
    const sorted = headers.filter((header) => header.getAttribute("aria-sort") !== "none");
    expect(sorted).toHaveLength(1);
    expect(sorted[0]).toHaveAttribute("aria-sort", "descending");
    expect(sorted[0]).toHaveTextContent(tr("Last modified"));
  });

  it("resizes a column with the keyboard, saves the width and restores it at start", async () => {
    const { user, mock, unmount } = await renderApp();
    await openBucket(user, "demo-photos");
    const handle = screen.getByRole("separator", { name: tr("Resize the {column} column", { column: tr("Size") }) });
    expect(handle).toHaveAttribute("aria-valuenow", "96");
    handle.focus();
    await user.keyboard("{ArrowLeft}{ArrowLeft}");
    expect(handle).toHaveAttribute("aria-valuenow", "112");
    await waitFor(async () => expect((await mock.get_settings()).interface.column_widths).toEqual({ size: 112, modified: 160, class: 140 }));
    expect(screen.getByRole("grid", { name: tr("Objects") }).style.getPropertyValue("--bb-col-size")).toBe("112px");

    unmount();
    const restored = await renderApp({
      prepare: (backend) => storedSettings(backend, (draft) => {
        draft.interface.column_widths = { size: 200, modified: 90, class: 300 };
      }),
    });
    await openBucket(restored.user, "demo-photos");
    const grid = screen.getByRole("grid", { name: tr("Objects") });
    expect(grid.style.getPropertyValue("--bb-col-size")).toBe("200px");
    expect(grid.style.getPropertyValue("--bb-col-modified")).toBe("90px");
    expect(grid.style.getPropertyValue("--bb-col-class")).toBe("300px");
  });

  it("drags a column wider with the pointer and keeps it inside 40 to 600 pixels", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const handle = screen.getByRole("separator", { name: tr("Resize the {column} column", { column: tr("Last modified") }) });
    fireEvent.pointerDown(handle, { clientX: 500, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 450, pointerId: 1 });
    expect(handle).toHaveAttribute("aria-valuenow", "210");
    fireEvent.pointerMove(handle, { clientX: 5000, pointerId: 1 });
    expect(handle).toHaveAttribute("aria-valuenow", "40");
    fireEvent.pointerMove(handle, { clientX: -5000, pointerId: 1 });
    fireEvent.pointerUp(handle, { clientX: -5000, pointerId: 1 });
    await waitFor(async () => expect((await mock.get_settings()).interface.column_widths.modified).toBe(600));
  });
});

describe("bucket list", () => {
  const manyBuckets = async (mock: MockBackend) => {
    const accountId = (await mock.list_accounts()).default_account_id;
    if (accountId === null) {
      throw new Error("no default account");
    }
    for (let n = 1; n <= 12; n += 1) {
      await mock.create_bucket({ account_id: accountId, name: `bulk-${n}`, region: n % 2 === 0 ? "eu-west-1" : null });
    }
    await storedSettings(mock, (draft) => {
      draft.interface.bucket_page_size = 10;
    });
  };

  it("sorts bucket names naturally and shows them a page at a time", async () => {
    const { user } = await renderApp({ prepare: manyBuckets });
    await screen.findByText("bulk-1");
    const names = treeNames();
    expect(names).toHaveLength(10);
    expect(names.slice(0, 3)).toEqual(["bulk-1", "bulk-2", "bulk-3"]);
    expect(screen.getByText(tr("Showing {shown} of {total} buckets", { shown: "10", total: "17" }))).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: tr("Show more buckets") }));
    expect(treeNames()).toHaveLength(17);
    expect(treeNames().indexOf("bulk-10")).toBeGreaterThan(treeNames().indexOf("bulk-9"));
    expect(screen.queryByRole("button", { name: tr("Show more buckets") })).toBeNull();
  });

  it("filters by name and region, and clears the filters", async () => {
    const { user } = await renderApp({ prepare: manyBuckets });
    await screen.findByText("bulk-1");
    await user.type(screen.getByRole("textbox", { name: tr("Filter buckets") }), "BULK-1");
    // bulk-1, bulk-10, bulk-11, bulk-12
    expect(treeNames()).toEqual(["bulk-1", "bulk-10", "bulk-11", "bulk-12"]);
    expect(screen.getByText(tr("{matching} of {total} buckets match", { matching: "4", total: "17" }))).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: tr("Filter by region and creation date") }));
    await user.selectOptions(await screen.findByRole("combobox", { name: tr("Region") }), "eu-west-1");
    expect(treeNames()).toEqual(["bulk-10", "bulk-12"]);

    await user.click(screen.getByRole("button", { name: tr("Clear filters") }));
    expect(screen.queryByText(tr("Clear filters"))).toBeNull();
    expect(treeNames()).toHaveLength(10);
  });

  it("filters by creation date", async () => {
    const { user } = await renderApp({ prepare: manyBuckets });
    await screen.findByText("bulk-1");
    await user.click(screen.getByRole("button", { name: tr("Filter by region and creation date") }));
    // demo-photos was created 300 days before the demo date; the bulk buckets are created now.
    fireEvent.change(await screen.findByLabelText(tr("Created on or before")), { target: { value: "2026-01-01" } });
    await waitFor(() => expect(treeNames()).not.toContain("bulk-1"));
    expect(treeNames()).toContain("demo-photos");
    fireEvent.change(screen.getByLabelText(tr("Created on or after")), { target: { value: "2027-01-01" } });
    await waitFor(() => expect(screen.getByText(tr("{matching} of {total} buckets match", { matching: "0", total: "17" }))).toBeInTheDocument());
  });
});
