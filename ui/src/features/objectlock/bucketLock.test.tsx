import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { renderApp } from "../../test/renderApp";

async function openLockDialog(user: UserEvent, bucket: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
  const row = (await within(tree).findByText(bucket)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${bucket}`);
  }
  fireEvent.contextMenu(row);
  await user.click(screen.getByRole("menuitem", { name: tr("Properties") }));
  await user.click(within(screen.getByRole("menu", { name: tr("Properties") })).getByRole("menuitem", { name: tr("Object lock…") }));
  return screen.findByRole("dialog", { name: tr("Object lock: {bucket}", { bucket }) });
}

describe("turn on Object Lock for an existing bucket", () => {
  it("needs versioning first: it offers to enable it, then asks for the bucket name", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: (await mock.list_accounts()).default_account_id ?? "", bucket: "demo-empty" };
    expect((await mock.get_bucket_versioning(ref)).status).toBe("unversioned");
    const dialog = await openLockDialog(user, "demo-empty");

    // Versioning has never been enabled: the lock cannot be turned on yet.
    expect(await within(dialog).findByText(tr("Versioning is needed first"))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("Object Lock needs versioning, and versioning has never been enabled on this bucket."))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Turn on Object Lock") })).toBeDisabled();

    await user.click(within(dialog).getByRole("button", { name: tr("Enable versioning") }));
    await waitFor(async () => expect((await mock.get_bucket_versioning(ref)).status).toBe("enabled"));
    expect(await within(dialog).findByText(tr("This cannot be undone"))).toBeInTheDocument();
    const turnOn = within(dialog).getByRole("button", { name: tr("Turn on Object Lock") });
    await waitFor(() => expect(turnOn).toBeEnabled());

    // The strong confirmation wants the bucket name; a wrong name keeps it disabled.
    await user.click(turnOn);
    const confirm = await screen.findByRole("dialog", { name: tr("Turn on Object Lock") });
    expect(confirm).toHaveTextContent(tr("Object Lock cannot be turned off again once it is on for {bucket}, by you or by anyone else. With Compliance retention, protected object versions cannot be deleted until their retention ends. Versioning must be enabled.", { bucket: "demo-empty" }));
    const confirmButton = within(confirm).getByRole("button", { name: tr("Turn on") });
    expect(confirmButton).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "demo-emptyy");
    expect(confirmButton).toBeDisabled();
    await user.clear(within(confirm).getByRole("textbox"));
    await user.type(within(confirm).getByRole("textbox"), "demo-empty");
    expect((await mock.get_bucket_object_lock(ref)).enabled).toBe(false);
    await user.click(confirmButton);
    await waitFor(async () => expect((await mock.get_bucket_object_lock(ref)).enabled).toBe(true));
  });

  it("goes straight to the confirmation on a versioned bucket, and a cancelled confirmation changes nothing", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: (await mock.list_accounts()).default_account_id ?? "", bucket: "demo-backups" };
    expect((await mock.get_bucket_versioning(ref)).status).toBe("enabled");
    const dialog = await openLockDialog(user, "demo-backups");
    expect(await within(dialog).findByText(tr("This cannot be undone"))).toBeInTheDocument();
    expect(within(dialog).queryByText(tr("Versioning is needed first"))).not.toBeInTheDocument();
    const turnOn = within(dialog).getByRole("button", { name: tr("Turn on Object Lock") });
    await waitFor(() => expect(turnOn).toBeEnabled());
    await user.click(turnOn);
    const confirm = await screen.findByRole("dialog", { name: tr("Turn on Object Lock") });
    await user.click(within(confirm).getByRole("button", { name: tr("Cancel") }));
    expect((await mock.get_bucket_object_lock(ref)).enabled).toBe(false);
  });

  it("does not warn about a bucket whose lock is already on", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: (await mock.list_accounts()).default_account_id ?? "", bucket: "demo-backups" };
    await mock.put_bucket_object_lock({ ...ref, default_retention: null });
    const dialog = await openLockDialog(user, "demo-backups");
    expect(await within(dialog).findByText(tr("Object Lock is on for this bucket."))).toBeInTheDocument();
    expect(within(dialog).queryByText(tr("This cannot be undone"))).not.toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Save") })).toBeEnabled();
  });
});
