import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { findObjectRow, openBucket, renderApp, statusBar } from "../../test/renderApp";
import { destinationProblem } from "./problem";

describe("destinationProblem", () => {
  it("refuses a folder moved or copied into itself or a subfolder", () => {
    expect(destinationProblem("move", "b", ["a/"], "b", "a/sub/")).toBe(
      "A folder cannot be moved into itself or one of its subfolders.",
    );
    expect(destinationProblem("copy", "b", ["a/"], "b", "a/")).toBe(
      "A folder cannot be copied into itself or one of its subfolders.",
    );
    expect(destinationProblem("move", "b", ["a/"], "other", "a/")).toBeNull();
    expect(destinationProblem("move", "b", ["a/"], null, "")).toBe("Choose a bucket first.");
  });
});

async function openMenuOn(name: string, menuName: string) {
  fireEvent.contextMenu(await findObjectRow(name));
  return screen.findByRole("menu", { name: menuName });
}

describe("destination picker", () => {
  it("copies a file to a folder of another bucket chosen in the picker", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const menu = await openMenuOn("readme.md", "File menu");
    await user.click(within(menu).getByRole("menuitem", { name: "Copy to…" }));
    const dialog = await screen.findByRole("dialog", { name: "Copy 1 item to" });
    await user.click(await within(dialog).findByText("Buckets"));
    await user.click(await within(dialog).findByText("demo-backups"));
    await within(dialog).findByText("Destination: s3://demo-backups/");
    await user.click(within(dialog).getByRole("button", { name: "Copy here" }));
    expect(await statusBar().findByText("Copy: 1 task added to the transfer queue.")).toBeInTheDocument();
    await advance(1500);
    expect(mock.objectKeys("demo-backups")).toContain("readme.md");
    expect(mock.objectKeys("demo-photos")).toContain("readme.md");
  });

  it("moves a file into a new folder created in the picker", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const menu = await openMenuOn("readme.md", "File menu");
    await user.click(within(menu).getByRole("menuitem", { name: "Move to…" }));
    const dialog = await screen.findByRole("dialog", { name: "Move 1 item to" });
    await within(dialog).findByText("archive");
    await user.click(within(dialog).getByRole("button", { name: "New folder" }));
    await user.type(within(dialog).getByRole("textbox", { name: "New folder name" }), "fresh");
    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    await within(dialog).findByText("Destination: s3://demo-photos/fresh/");
    await user.click(within(dialog).getByRole("button", { name: "Move here" }));
    expect(await statusBar().findByText("Move: 1 task added to the transfer queue.")).toBeInTheDocument();
    await advance(1500);
    expect(mock.objectKeys("demo-photos")).toContain("fresh/readme.md");
    expect(mock.objectKeys("demo-photos")).not.toContain("readme.md");
  });

  it("refuses to move a folder into itself with a clear message", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const menu = await openMenuOn("documents", "Folder menu");
    await user.click(within(menu).getByRole("menuitem", { name: "Move all files…" }));
    const dialog = await screen.findByRole("dialog", { name: "Move 1 item to" });
    await user.click(await within(dialog).findByText("documents"));
    expect(await within(dialog).findByText("A folder cannot be moved into itself or one of its subfolders.")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Move here" })).toBeDisabled();
  });

  it("copies a whole folder to another bucket", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const menu = await openMenuOn("documents", "Folder menu");
    await user.click(within(menu).getByRole("menuitem", { name: "Copy all files…" }));
    const dialog = await screen.findByRole("dialog", { name: "Copy 1 item to" });
    await user.click(await within(dialog).findByText("Buckets"));
    await user.click(await within(dialog).findByText("demo-backups"));
    await user.click(within(dialog).getByRole("button", { name: "Copy here" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Copy 1 item to" })).not.toBeInTheDocument());
    await advance(1500);
    expect(mock.objectKeys("demo-backups").some((key) => key.startsWith("documents/"))).toBe(true);
  });

  it("pastes the clipboard into the root of a bucket from the bucket menu", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{Control>}c{/Control}");
    const tree = await screen.findByRole("tree", { name: "Buckets and folders" });
    const row = (await within(tree).findByText("demo-backups")).closest<HTMLElement>('[role="treeitem"]');
    fireEvent.contextMenu(row!);
    const menu = await screen.findByRole("menu", { name: "Bucket menu" });
    await user.click(within(menu).getByRole("menuitem", { name: "Paste" }));
    expect(await statusBar().findByText("Copy: 1 task added to the transfer queue.")).toBeInTheDocument();
    await advance(1500);
    expect(mock.objectKeys("demo-backups")).toContain("readme.md");
  });
});
