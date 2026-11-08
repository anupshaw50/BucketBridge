import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { PendingConflict } from "../../api/types";
import { tr } from "../../i18n";
import { ConflictStore } from "../../state/conflicts";
import { findObjectRow, openBucket, renderApp, statusBar } from "../../test/renderApp";
import { destinationProblem } from "../picker/problem";

/** Escapes a string for use in a RegExp. */
const esc = (text: string): string => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

async function chooseCopyTo(user: Awaited<ReturnType<typeof renderApp>>["user"], bucketName: string) {
  fireEvent.contextMenu(await findObjectRow("readme.md"));
  const menu = await screen.findByRole("menu", { name: tr("File menu") });
  await user.click(within(menu).getByRole("menuitem", { name: tr("Copy to…") }));
  const dialog = await screen.findByRole("dialog", { name: tr("Copy 1 item to") });
  await user.click(await within(dialog).findByText(tr("Buckets")));
  await user.click(await within(dialog).findByText(bucketName));
  return dialog;
}

async function pickPolicy(user: Awaited<ReturnType<typeof renderApp>>["user"], dialog: HTMLElement, label: string) {
  await user.click(within(dialog).getByRole("combobox", { name: tr("If an item already exists there") }));
  await user.click(await screen.findByRole("option", { name: label }));
}

const conflict: PendingConflict = {
  task_id: "t1",
  group_id: "g1",
  conflict: {
    kind: "upload",
    source: "/tmp/a.txt",
    destination: "s3://b/a.txt",
    source_info: { size: 3, modified: null },
    destination_info: { size: 5, modified: null },
  },
};

describe("conflict store", () => {
  it("keeps the questions of waiting tasks and drops them when they are resolved", () => {
    const store = new ConflictStore();
    store.onEvent({ type: "asked", ...conflict });
    store.onEvent({ type: "asked", ...conflict, task_id: "t2" });
    store.onEvent({ type: "asked", ...conflict });
    expect(store.store.getState().pending.map((item) => item.task_id)).toEqual(["t2", "t1"]);
    store.mergeSnapshot([conflict, { ...conflict, task_id: "t3" }]);
    expect(store.store.getState().pending.map((item) => item.task_id)).toEqual(["t2", "t1", "t3"]);
    store.onEvent({ type: "resolved", task_id: "t2" });
    store.onEvent({ type: "resolved", task_id: "unknown" });
    expect(store.store.getState().pending.map((item) => item.task_id)).toEqual(["t1", "t3"]);
  });
});

describe("destinationProblem between accounts", () => {
  it("allows the same bucket name and folder paths in another account, and still refuses name clashes", () => {
    expect(destinationProblem("copy", "b", ["a/"], "b", "a/", false)).toBeNull();
    expect(destinationProblem("move", "b", ["x/f.txt", "y/f.txt"], "other", "", false)).toBe(
      tr("Two or more selected items have the same name \"{name}\" and would {verb} to the same place. Select them separately.", {
        name: "f.txt",
        verb: tr("be moved"),
      }),
    );
    expect(destinationProblem("copy", "b", [], "other", "", false)).toBe(tr("Nothing is selected."));
  });
});

describe("overwrite policy in the app", () => {
    it("skips an item that exists, and says so in the transfer queue", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    // The first copy creates readme.md in demo-backups.
    let dialog = await chooseCopyTo(user, "demo-backups");
    await user.click(within(dialog).getByRole("button", { name: tr("Copy here") }));
    await advance(1500);
    expect(mock.objectKeys("demo-backups")).toContain("readme.md");
    // The second one skips it.
    dialog = await chooseCopyTo(user, "demo-backups");
    await pickPolicy(user, dialog, tr("Skip"));
    await user.click(within(dialog).getByRole("button", { name: tr("Copy here") }));
    await advance(1500);
    const panel = await screen.findByRole("region", { name: tr("Transfer queue") });
    expect(await within(panel).findByText(tr("Skipped"))).toBeInTheDocument();
    expect(within(panel).getByText(tr("Skipped: {reason}", { reason: "the destination already exists" }))).toBeInTheDocument();
  });

  it("renames when asked to", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    let dialog = await chooseCopyTo(user, "demo-backups");
    await user.click(within(dialog).getByRole("button", { name: tr("Copy here") }));
    await advance(1500);
    dialog = await chooseCopyTo(user, "demo-backups");
    await pickPolicy(user, dialog, tr("Rename (name (1).ext)"));
    await user.click(within(dialog).getByRole("button", { name: tr("Copy here") }));
    await advance(1500);
    expect(mock.objectKeys("demo-backups")).toEqual(expect.arrayContaining(["readme.md", "readme (1).md"]));
  });

  it("waits for an answer when asked to ask, and the dialog decides", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    let dialog = await chooseCopyTo(user, "demo-backups");
    await user.click(within(dialog).getByRole("button", { name: tr("Copy here") }));
    await advance(1500);
    dialog = await chooseCopyTo(user, "demo-backups");
    await pickPolicy(user, dialog, tr("Ask me"));
    await user.click(within(dialog).getByRole("button", { name: tr("Copy here") }));
    await advance(500);
    const question = await screen.findByRole("dialog", { name: tr("The destination already exists") });
    expect(within(question).getByText("s3://demo-backups/readme.md")).toBeInTheDocument();
    // The task waits: nothing was written and the queue says so.
    const panel = screen.getByRole("region", { name: tr("Transfer queue") });
    expect(await within(panel).findByText(tr("Waiting"))).toBeInTheDocument();
    await user.click(within(question).getByRole("button", { name: tr("Skip") }));
    await advance(500);
    expect(screen.queryByRole("dialog", { name: tr("The destination already exists") })).not.toBeInTheDocument();
    expect(await within(panel).findByText(tr("Skipped"))).toBeInTheDocument();
    expect(mock.objectKeys("demo-backups")).not.toContain("readme (1).md");
  });

  it("copies to a bucket of another account chosen in the picker", async () => {
    const { user, advance, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    const menu = await screen.findByRole("menu", { name: tr("File menu") });
    await user.click(within(menu).getByRole("menuitem", { name: tr("Copy to…") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Copy 1 item to") });
    await user.click(within(dialog).getByRole("combobox", { name: tr("Destination account") }));
    await user.click(await screen.findByRole("option", { name: "Archive (custom endpoint)" }));
    await user.click(await within(dialog).findByText("archive-media"));
    await waitFor(() => expect(dialog.textContent).toContain(tr("Destination:")));
    expect(
      within(dialog).getByText(tr("Between accounts the service copies the objects when it can; otherwise they go through this computer.")),
    ).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Copy here") }));
    expect(
      await statusBar().findByText(tr("{what}: 1 task added to the transfer queue.", { what: tr("Copy") })),
    ).toBeInTheDocument();
    await advance(1500);
    expect(mock.objectKeys("archive-media")).toContain("readme.md");
    expect(mock.objectKeys("demo-photos")).toContain("readme.md");
  });

  it("changes the default policy and the modification-time switch in the settings", async () => {
    const { user, mock } = await renderApp();
    await user.click(await screen.findByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("button", { name: tr("Transfers") }));
    await user.click(within(dialog).getByRole("combobox", { name: tr("When the destination exists") }));
    await user.click(await screen.findByRole("option", { name: tr("Skip") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Data integrity") }));
    await user.click(within(dialog).getByRole("switch", { name: new RegExp(esc(tr("Keep modification times"))) }));
    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    const saved = await mock.get_settings();
    expect(saved.transfer.if_exists).toBe("skip");
    expect(saved.data_integrity.preserve_mtime).toBe(false);
  });
});
