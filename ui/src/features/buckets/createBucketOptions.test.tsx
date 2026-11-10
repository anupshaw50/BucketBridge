import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import type { MockBackend } from "../../mock/mockBackend";
import { renderApp } from "../../test/renderApp";

async function openDialog(user: UserEvent): Promise<HTMLElement> {
  await screen.findByText("demo-photos");
  await user.click(screen.getByRole("button", { name: tr("New bucket") }));
  return screen.findByRole("dialog", { name: tr("New bucket") });
}

async function setName(user: UserEvent, dialog: HTMLElement, name: string): Promise<void> {
  await user.type(within(dialog).getByRole("textbox", { name: new RegExp(tr("Bucket name")) }), name);
}

async function reference(mock: MockBackend, bucket: string): Promise<{ account_id: string; bucket: string }> {
  return { account_id: (await mock.list_accounts()).default_account_id ?? "", bucket };
}

describe("create bucket: advanced options", () => {
  it("sends no options until the advanced section is opened", async () => {
    const { user, mock } = await renderApp();
    const dialog = await openDialog(user);
    await setName(user, dialog, "plain-bucket");
    expect(within(dialog).getByRole("button", { name: tr("Advanced options") })).toHaveAttribute("aria-expanded", "false");
    await user.click(within(dialog).getByRole("button", { name: tr("Create bucket") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("New bucket") })).not.toBeInTheDocument());
    const ref = await reference(mock, "plain-bucket");
    expect(await mock.get_object_ownership(ref)).toBeNull();
    expect(await mock.get_public_access_block(ref)).toBeNull();
    expect((await mock.get_bucket_object_lock(ref)).enabled).toBe(false);
  });

  it("starts with all four public access switches on and creates the bucket with them", async () => {
    const { user, mock } = await renderApp();
    const dialog = await openDialog(user);
    await setName(user, dialog, "blocked-bucket");
    await user.click(within(dialog).getByRole("button", { name: tr("Advanced options") }));
    for (const name of [
      "Block new public ACLs",
      "Ignore public ACLs",
      "Block new public bucket policies",
      "Restrict access to buckets with a public policy",
    ]) {
      expect(within(dialog).getByRole("switch", { name: new RegExp(tr(name)) })).toBeChecked();
    }
    await user.click(within(dialog).getByRole("button", { name: tr("Create bucket") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("New bucket") })).not.toBeInTheDocument());
    expect(await mock.get_public_access_block(await reference(mock, "blocked-bucket"))).toEqual({
      block_public_acls: true,
      ignore_public_acls: true,
      block_public_policy: true,
      restrict_public_buckets: true,
    });
  });

  it("creates a bucket with ownership, a public access switch off and Object Lock after a confirmation", async () => {
    const { user, mock } = await renderApp();
    const dialog = await openDialog(user);
    await setName(user, dialog, "locked-bucket");
    await user.click(within(dialog).getByRole("button", { name: tr("Advanced options") }));
    await user.click(within(dialog).getByRole("combobox", { name: new RegExp(tr("Object ownership")) }));
    await user.click(await within(dialog).findByRole("option", { name: tr("Bucket owner preferred (ACLs enabled)") }));
    await user.click(within(dialog).getByRole("switch", { name: new RegExp(tr("Block new public bucket policies")) }));
    expect(within(dialog).getByText(tr("With a switch off, this bucket can become public through ACLs or a bucket policy."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("switch", { name: new RegExp(tr("Enable Object Lock")) }));

    // Cancelling the confirmation creates nothing.
    await user.click(within(dialog).getByRole("button", { name: tr("Create bucket") }));
    const confirm = await screen.findByRole("dialog", { name: tr("Turn on Object Lock") });
    await user.click(within(confirm).getByRole("button", { name: tr("Cancel") }));
    const ref = await reference(mock, "locked-bucket");
    await expect(mock.get_bucket_versioning(ref)).rejects.toMatchObject({ code: "not_found" });

    await user.click(within(dialog).getByRole("button", { name: tr("Create bucket") }));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Turn on Object Lock") })).getByRole("button", { name: tr("Create bucket") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("New bucket") })).not.toBeInTheDocument());
    expect(await mock.get_object_ownership(ref)).toBe("BucketOwnerPreferred");
    expect((await mock.get_bucket_object_lock(ref)).enabled).toBe(true);
    expect((await mock.get_bucket_versioning(ref)).status).toBe("enabled");
    expect(await mock.get_public_access_block(ref)).toEqual({
      block_public_acls: true,
      ignore_public_acls: true,
      block_public_policy: false,
      restrict_public_buckets: true,
    });
  });
});
