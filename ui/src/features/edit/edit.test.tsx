import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { EditSession } from "../../api/types";
import { tr } from "../../i18n";
import { copyKey, editStateLabel } from "../../lib/edit";
import type { MockBackend } from "../../mock/mockBackend";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";

const BUCKET = "demo-photos";
const KEY = "readme.md";

/** Starts editing readme.md through its context menu. */
async function startEditing(user: Awaited<ReturnType<typeof renderApp>>["user"], mock: MockBackend): Promise<EditSession> {
  await openBucket(user, BUCKET);
  fireEvent.contextMenu(await findObjectRow(KEY));
  await user.click(await screen.findByRole("menuitem", { name: tr("Edit") }));
  await waitFor(async () => expect(await mock.list_edits()).toHaveLength(1));
  const [session] = await mock.list_edits();
  if (session === undefined) {
    throw new Error("no session");
  }
  return session;
}

describe("edit helpers", () => {
  it("names the copy next to the original and the states", () => {
    expect(copyKey("docs/report.txt")).toBe("docs/report (edited).txt");
    expect(copyKey("report.txt", 3)).toBe("report (edited 3).txt");
    expect(copyKey("Makefile")).toBe("Makefile (edited)");
    expect(copyKey(".env")).toBe(".env (edited)");
    expect(editStateLabel("conflict")).toBe(tr("Conflict"));
  });
});

describe("Edit menu entry", () => {
  it("is offered for one object only and starts a session that shows in the status bar", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, BUCKET);

    fireEvent.contextMenu(await findObjectRow(KEY));
    expect(await screen.findByRole("menuitem", { name: tr("Edit") })).toBeInTheDocument();
    await user.keyboard("{Escape}");

    // Several selected objects: no Edit entry.
    await user.click(await findObjectRow(KEY));
    await user.keyboard("{Control>}a{/Control}");
    fireEvent.contextMenu(await findObjectRow(KEY));
    await screen.findByRole("menu", { name: tr("Selection menu") });
    expect(screen.queryByRole("menuitem", { name: tr("Edit") })).toBeNull();
    await user.keyboard("{Escape}");

    await user.click(await findObjectRow(KEY));
    fireEvent.contextMenu(await findObjectRow(KEY));
    await user.click(await screen.findByRole("menuitem", { name: tr("Edit") }));
    const indicator = await screen.findByRole("button", { name: "Editing 1 object" });
    expect(indicator).toHaveTextContent(tr("Editing {n}", { n: 1 }));
    const [session] = await mock.list_edits();
    expect(session?.key).toBe(KEY);
    expect(session?.state).toBe("watching");
  });
});

describe("Editing panel", () => {
  it("lists sessions with their state and stops one", async () => {
    const { user, mock } = await renderApp();
    const session = await startEditing(user, mock);

    await user.click(await screen.findByRole("button", { name: "Editing 1 object" }));
    const dialog = await screen.findByRole("dialog", { name: tr("Editing") });
    const table = within(dialog).getByRole("table", { name: tr("Objects being edited") });
    expect(within(table).getByText(KEY)).toBeInTheDocument();
    expect(within(table).getByText(tr("Waiting for a save"))).toBeInTheDocument();

    act(() => mock.saveEdit(session.id));
    expect(await within(table).findByText(tr("Uploaded"))).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("Stop editing {key}", { key: KEY }) }));
    expect(await within(dialog).findByText(tr("No objects are being edited."))).toBeInTheDocument();
    expect(await mock.list_edits()).toHaveLength(0);
    expect(screen.queryByRole("button", { name: /^Editing \d/ })).toBeNull();
  });

  it("shows a failed stop and keeps the session while a conflict stands", async () => {
    const { user, mock } = await renderApp();
    const session = await startEditing(user, mock);
    mock.changeObject(session.account_id, BUCKET, KEY);
    act(() => mock.saveEdit(session.id));
    // The conflict dialog opens by itself; close it and use the panel.
    const conflict = await screen.findByRole("dialog", { name: tr("Edit conflict") });
    await user.click(within(conflict).getByRole("button", { name: tr("Decide later") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Edit conflict") })).toBeNull());

    await user.click(await screen.findByRole("button", { name: /^Editing 1 object/ }));
    const dialog = await screen.findByRole("dialog", { name: tr("Editing") });
    expect(within(dialog).getByText(tr("Conflict"))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Stop editing {key}", { key: KEY }) }));
    expect(await mock.list_edits()).toHaveLength(1);
    // Resolve… reopens the conflict dialog.
    await user.click(within(dialog).getByRole("button", { name: tr("Resolve…") }));
    expect(await screen.findByRole("dialog", { name: tr("Edit conflict") })).toBeInTheDocument();
  });
});

describe("Edit conflict dialog", () => {
  it("opens when a save finds the object changed and overwrites after a confirmation", async () => {
    const { user, mock } = await renderApp();
    const session = await startEditing(user, mock);
    const before = (await mock.list_objects({ account_id: session.account_id, bucket: BUCKET, prefix: "", flat: true, token: null })).entries.find(
      (entry) => entry.key === KEY,
    );
    mock.changeObject(session.account_id, BUCKET, KEY);
    act(() => mock.saveEdit(session.id));

    const dialog = await screen.findByRole("dialog", { name: tr("Edit conflict") });
    expect(within(dialog).getByText(new RegExp(tr("{name} was changed on the server after you opened it.", { name: "s3://demo-photos/readme.md" }).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")))).toBeInTheDocument();
    expect(within(dialog).getByText(/readme \(edited\)\.md/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Overwrite") }));
    // The confirmation comes first; declining changes nothing.
    let confirm = await screen.findByRole("dialog", { name: tr("Overwrite the object") });
    await user.click(within(confirm).getByRole("button", { name: tr("Cancel") }));
    expect((await mock.list_edits())[0]?.state).toBe("conflict");

    await user.click(within(dialog).getByRole("button", { name: tr("Overwrite") }));
    confirm = await screen.findByRole("dialog", { name: tr("Overwrite the object") });
    await user.click(within(confirm).getByRole("button", { name: tr("Overwrite") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Edit conflict") })).toBeNull());
    const [after] = await mock.list_edits();
    expect(after?.state).toBe("uploaded");
    expect(after?.uploads).toBe(1);
    const listed = (await mock.list_objects({ account_id: session.account_id, bucket: BUCKET, prefix: "", flat: true, token: null })).entries.find(
      (entry) => entry.key === KEY,
    );
    expect(listed?.etag).toBe(after?.etag);
    expect(listed?.etag).not.toBe(before?.etag);
  });

  it("saves the local version as a copy and ends the session", async () => {
    const { user, mock } = await renderApp();
    const session = await startEditing(user, mock);
    mock.changeObject(session.account_id, BUCKET, KEY);
    act(() => mock.saveEdit(session.id));

    const dialog = await screen.findByRole("dialog", { name: tr("Edit conflict") });
    await user.click(within(dialog).getByRole("button", { name: tr("Save as a copy") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Edit conflict") })).toBeNull());
    expect(await mock.list_edits()).toHaveLength(0);
    expect(mock.objectKeys(BUCKET)).toContain("readme (edited).md");
  });

  it("discards the changes after a confirmation", async () => {
    const { user, mock } = await renderApp();
    const session = await startEditing(user, mock);
    mock.changeObject(session.account_id, BUCKET, KEY);
    act(() => mock.saveEdit(session.id));

    const dialog = await screen.findByRole("dialog", { name: tr("Edit conflict") });
    await user.click(within(dialog).getByRole("button", { name: tr("Discard my changes") }));
    const confirm = await screen.findByRole("dialog", { name: tr("Discard your changes") });
    await user.click(within(confirm).getByRole("button", { name: tr("Discard") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Edit conflict") })).toBeNull());
    expect(await mock.list_edits()).toHaveLength(0);
    expect(mock.objectKeys(BUCKET)).not.toContain("readme (edited).md");
  });

  it("says when the object was deleted and offers to create it again", async () => {
    const { user, mock } = await renderApp();
    const session = await startEditing(user, mock);
    mock.deleteObject(session.account_id, BUCKET, KEY);
    act(() => mock.saveEdit(session.id));

    const dialog = await screen.findByRole("dialog", { name: tr("Edit conflict") });
    expect(within(dialog).getByText(new RegExp(tr("{name} was deleted on the server after you opened it.", { name: "s3://demo-photos/readme.md" }).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("Creates the object again from your version."))).toBeInTheDocument();
  });
});
