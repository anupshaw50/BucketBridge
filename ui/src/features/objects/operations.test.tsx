import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MOCK_HOME } from "../../mock/world";
import { findObjectRow, openBucket, queryObjectRow, renderApp, statusBar, TestPlatform } from "../../test/renderApp";

function transferRow(text: RegExp): HTMLElement {
  const list = screen.getByRole("list", { name: "Transfers" });
  const row = within(list)
    .getAllByRole("listitem")
    .find((item) => text.test(item.getAttribute("aria-label") ?? ""));
  if (!row) {
    throw new Error(`no transfer matching ${String(text)}`);
  }
  return row;
}

describe("uploads and the transfer queue", () => {
  it("enqueues an upload, shows live progress, pauses, resumes and completes", async () => {
    const platform = new TestPlatform();
    platform.fileAnswers.push([`${MOCK_HOME}/Documents/report.pdf`]);
    const { user, advance } = await renderApp({ platform, mock: { bytesPerSecond: 1024 * 1024 } });
    await openBucket(user, "demo-backups");
    await user.click(screen.getByRole("button", { name: "Upload" }));
    await user.click(await screen.findByRole("menuitem", { name: "Upload files..." }));
    expect(await statusBar().findByText("Upload: 1 task added to the transfer queue.")).toBeInTheDocument();

    const panel = screen.getByRole("region", { name: "Transfer queue" });
    let row = transferRow(/report\.pdf/);
    expect(within(row).getByText("Queued")).toBeInTheDocument();
    expect(within(panel).getByRole("radio", { name: /Queued 1/ })).toBeInTheDocument();

    await advance(250);
    row = transferRow(/report\.pdf/);
    expect(within(row).getByText("Running")).toBeInTheDocument();
    expect(within(row).getByText(/256 KiB of 1\.2 MiB/)).toBeInTheDocument();
    expect(within(row).getByText(/left/)).toBeInTheDocument();
    expect(within(row).getByRole("button", { name: "Resume transfer" })).toBeDisabled();

    await user.click(within(row).getByRole("button", { name: "Pause transfer" }));
    await waitFor(() => expect(within(transferRow(/report\.pdf/)).getByText("Stopped")).toBeInTheDocument());
    row = transferRow(/report\.pdf/);
    expect(within(row).getByRole("button", { name: "Pause transfer" })).toBeDisabled();
    await advance(1000);
    expect(within(transferRow(/report\.pdf/)).getByText(/256 KiB of/)).toBeInTheDocument();

    await user.click(within(row).getByRole("button", { name: "Resume transfer" }));
    await waitFor(() => expect(within(transferRow(/report\.pdf/)).getByText("Queued")).toBeInTheDocument());
    await advance(2000);
    row = transferRow(/report\.pdf/);
    expect(within(row).getByText("Completed")).toBeInTheDocument();
    await advance(1000);
    expect(await findObjectRow("report.pdf")).toBeInTheDocument();
  });

  it("filters the queue by view, retries failures and clears finished tasks", async () => {
    const { user, advance } = await renderApp();
    const panel = await screen.findByRole("region", { name: "Transfer queue" });
    await user.click(within(panel).getByRole("radio", { name: /Failed 1/ }));
    const failed = transferRow(/unreadable-raw/);
    expect(within(failed).getByText("Permission denied (os error 13)")).toBeInTheDocument();
    await user.click(within(failed).getByRole("button", { name: "Retry transfer" }));
    await waitFor(() => expect(within(panel).getByRole("radio", { name: /Failed 0/ })).toBeInTheDocument());
    await advance(250);
    expect(within(panel).getByRole("radio", { name: /Failed 1/ })).toBeInTheDocument();

    await user.click(within(panel).getByRole("radio", { name: /All 3/ }));
    await user.click(within(panel).getByRole("button", { name: "Start all" }));
    await advance(5000);
    await user.click(within(panel).getByRole("button", { name: "Clear finished" }));
    await waitFor(() => expect(within(panel).getByRole("radio", { name: /All 1/ })).toBeInTheDocument());
    expect(transferRow(/unreadable-raw/)).toBeInTheDocument();

    await user.click(within(panel).getByRole("button", { name: "Cancel all" }));
    const confirm = await screen.findByRole("dialog", { name: "Cancel all transfers" });
    await user.click(within(confirm).getByRole("button", { name: "Cancel transfers" }));
    await waitFor(() => expect(within(transferRow(/unreadable-raw/)).getByText("Cancelled")).toBeInTheDocument());
  });

  it("keeps the panel responsive with thousands of tasks", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Many files`);
    const { user } = await renderApp({ platform, mock: { manyFiles: 3000 } });
    await openBucket(user, "demo-backups");
    await user.click(screen.getByRole("button", { name: "Upload" }));
    await user.click(await screen.findByRole("menuitem", { name: "Upload folder..." }));
    const panel = screen.getByRole("region", { name: "Transfer queue" });
    expect(await within(panel).findByRole("radio", { name: /All 3,003/ })).toBeInTheDocument();
    const rendered = within(screen.getByRole("list", { name: "Transfers" })).getAllByRole("listitem");
    expect(rendered.length).toBeLessThan(60);
  });
});

describe("object operations", () => {
  it("deletes after confirmation and leaves things alone when cancelled", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    expect(screen.getByRole("grid", { name: "Objects" })).toHaveFocus();
    await user.keyboard("{Delete}");
    let confirm = await screen.findByRole("dialog", { name: "Delete" });
    expect(within(confirm).getByText(/Delete "readme.md" from demo-photos\?/)).toBeInTheDocument();
    await user.click(within(confirm).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Delete" })).not.toBeInTheDocument());
    expect(mock.objectKeys("demo-photos")).toContain("readme.md");

    await user.click(screen.getByRole("button", { name: "Delete" }));
    confirm = await screen.findByRole("dialog", { name: "Delete" });
    await user.click(within(confirm).getByRole("button", { name: "Delete" }));
    expect(await statusBar().findByText("Delete: 1 task added to the transfer queue.")).toBeInTheDocument();
    await advance(500);
    await advance(1000);
    await waitFor(() => expect(queryObjectRow("readme.md")).not.toBeInTheDocument());
    expect(mock.objectKeys("demo-photos")).not.toContain("readme.md");
  });

  it("deletes without asking when confirmation is turned off", async () => {
    const { user, mock } = await renderApp();
    await user.click(screen.getByRole("button", { name: "Settings" }));
    const dialog = await screen.findByRole("dialog", { name: "Settings" });
    await user.click(within(dialog).getByRole("switch", { name: /Ask before deleting/ }));
    await user.click(within(dialog).getByRole("button", { name: "OK" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Settings" })).not.toBeInTheDocument());
    expect((await mock.get_settings()).general.confirm_delete).toBe(false);
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.click(screen.getByRole("button", { name: "Delete" }));
    expect(await statusBar().findByText("Delete: 1 task added to the transfer queue.")).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Delete" })).not.toBeInTheDocument();
  });

  it("renames with F2, validates the name, and moves the object", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.dblClick(await findObjectRow("documents"));
    await user.click(await findObjectRow("notes.txt"));
    expect(screen.getByRole("grid", { name: "Objects" })).toHaveFocus();
    await user.keyboard("{F2}");
    const dialog = await screen.findByRole("dialog", { name: "Rename object" });
    const input = within(dialog).getByRole("textbox", { name: "New name" });
    expect(input).toHaveValue("notes.txt");
    const rename = within(dialog).getByRole("button", { name: "Rename" });
    expect(rename).toBeDisabled();
    await user.clear(input);
    await user.type(input, "report-q3.pdf");
    expect(within(dialog).getByText("An object with this name already exists.")).toBeInTheDocument();
    await user.clear(input);
    await user.type(input, "notes final.txt");
    await user.click(rename);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Rename object" })).not.toBeInTheDocument());
    await advance(500);
    await advance(1000);
    expect(await findObjectRow("notes final.txt")).toBeInTheDocument();
    expect(mock.objectKeys("demo-photos")).toContain("documents/notes final.txt");
    expect(mock.objectKeys("demo-photos")).not.toContain("documents/notes.txt");
  });

  it("copies and pastes into another folder, and refuses pasting into the same folder", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    expect(screen.getByRole("grid", { name: "Objects" })).toHaveFocus();
    await user.keyboard("{Control>}c{/Control}");
    expect(statusBar().getByText(/1 item copied/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Paste" })).toBeDisabled();
    await user.dblClick(await findObjectRow("archive"));
    await screen.findByText("This folder is empty");
    await user.click(screen.getByRole("button", { name: "Paste" }));
    expect(await statusBar().findByText("Copy: 1 task added to the transfer queue.")).toBeInTheDocument();
    await advance(500);
    await advance(1000);
    expect(await findObjectRow("readme.md")).toBeInTheDocument();
    expect(mock.objectKeys("demo-photos")).toEqual(expect.arrayContaining(["readme.md", "archive/readme.md"]));
  });

  it("cuts a folder and moves it, dimming the cut rows meanwhile", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("space folder"));
    expect(screen.getByRole("grid", { name: "Objects" })).toHaveFocus();
    await user.keyboard("{Meta>}x{/Meta}");
    expect(await findObjectRow("space folder")).toHaveAttribute("data-cut", "true");
    await user.dblClick(await findObjectRow("archive"));
    await screen.findByText("This folder is empty");
    await user.keyboard("{F5}");
    await user.click(screen.getByRole("button", { name: "Paste" }));
    expect(await statusBar().findByText("Move: 1 task added to the transfer queue.")).toBeInTheDocument();
    await advance(1500);
    expect(mock.objectKeys("demo-photos")).toContain("archive/space folder/hello world.txt");
    expect(mock.objectKeys("demo-photos")).not.toContain("space folder/hello world.txt");
    expect(screen.getByRole("button", { name: "Paste" })).toBeDisabled();
  });

  it("creates a folder", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await findObjectRow("readme.md");
    await user.click(screen.getByRole("button", { name: "New folder" }));
    const dialog = await screen.findByRole("dialog", { name: "New folder" });
    const input = within(dialog).getByRole("textbox", { name: "Folder name" });
    await user.type(input, "documents");
    expect(within(dialog).getByText("A folder with this name already exists.")).toBeInTheDocument();
    await user.clear(input);
    await user.type(input, "new stuff");
    await user.click(within(dialog).getByRole("button", { name: "Create folder" }));
    expect(await findObjectRow("new stuff")).toBeInTheDocument();
    expect(mock.objectKeys("demo-photos")).toContain("new stuff/");
  });

  it("creates a presigned link and copies it as a sensitive value", async () => {
    const { user, platform } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.click(screen.getByRole("button", { name: "Share link" }));
    const dialog = await screen.findByRole("dialog", { name: "Share link" });
    const amount = within(dialog).getByRole("textbox", { name: "Valid for" });
    await user.clear(amount);
    await user.type(amount, "8");
    await user.click(within(dialog).getByRole("combobox", { name: "Unit" }));
    await user.click(await within(dialog).findByRole("option", { name: "Days" }));
    expect(within(dialog).getByText("Choose an expiry between 1 second and 7 days.")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Create link" })).toBeDisabled();
    await user.clear(amount);
    await user.type(amount, "2");
    await user.click(within(dialog).getByRole("button", { name: "Create link" }));
    const link = await within(dialog).findByRole("textbox", { name: "Link" });
    expect((link as HTMLTextAreaElement).value).toContain("X-Amz-Expires=172800");
    await user.click(within(dialog).getByRole("button", { name: "Copy link" }));
    expect(platform.copied.at(-1)).toBe((link as HTMLTextAreaElement).value);
    expect(platform.copiedSensitive.at(-1)).toBe(true);
  });

  it("shows properties of an object", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.click(screen.getByRole("button", { name: "Properties" }));
    const dialog = await screen.findByRole("dialog", { name: "Properties" });
    expect(await within(dialog).findByText("1.0 KiB (1,024 bytes)")).toBeInTheDocument();
    expect(within(dialog).getByText("STANDARD")).toBeInTheDocument();
    const etag = within(dialog).getByRole("rowheader", { name: "ETag" }).closest("tr");
    expect(etag?.textContent).toMatch(/^ETag[0-9a-f]{32}$/);
    // The properties come from a HEAD request, which carries the stored checksums (listings do not).
    expect(within(dialog).getByRole("rowheader", { name: "Checksums" })).toBeInTheDocument();
    expect(within(dialog).getByText(/^CRC32: /)).toBeInTheDocument();
    expect(within(dialog).getByRole("textbox", { name: "S3 URI" })).toHaveValue("s3://demo-photos/readme.md");
  });
});
