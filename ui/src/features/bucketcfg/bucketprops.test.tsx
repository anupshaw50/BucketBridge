import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import type { MockBackend } from "../../mock/mockBackend";
import { renderApp, statusBar } from "../../test/renderApp";

async function demoAccount(mock: MockBackend): Promise<string> {
  const id = (await mock.list_accounts()).default_account_id;
  if (id === null) {
    throw new Error("no default account");
  }
  return id;
}

/** Opens a bucket and its properties from the bucket menu of the toolbar. */
async function openProperties(user: UserEvent, bucket: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
  await user.click(await within(tree).findByText(bucket));
  await screen.findByRole("grid", { name: tr("Objects") });
  await user.click(screen.getByRole("button", { name: tr("Bucket actions") }));
  await user.click(await screen.findByRole("menuitem", { name: tr("Properties of {name}...", { name: bucket }) }));
  return screen.findByRole("dialog", { name: tr("Bucket properties: {bucket}", { bucket }) });
}

async function selectTab(user: UserEvent, dialog: HTMLElement, name: string): Promise<void> {
  await user.click(within(dialog).getByRole("tab", { name }));
}

describe("bucket properties: default encryption", () => {
  it("shows no encryption, saves SSE-KMS with a key and the bucket key, then removes it", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: await demoAccount(mock), bucket: "demo-empty" };
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("Encryption"));
    expect(
      await within(dialog).findByText(tr("No default encryption is configured. On AWS, S3 then encrypts new objects with SSE-S3.")),
    ).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: tr("Remove configuration") })).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("radio", { name: new RegExp(tr("SSE-KMS")) }));
    await user.type(within(dialog).getByRole("textbox", { name: new RegExp(tr("KMS key")) }), "alias/team-key");
    await user.click(within(dialog).getByRole("switch", { name: new RegExp(tr("Use an S3 Bucket Key")) }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () =>
      expect(await mock.get_bucket_encryption(ref)).toEqual({ algorithm: "aws:kms", kms_key_id: "alias/team-key", bucket_key_enabled: true }),
    );
    expect(
      await within(dialog).findByText(tr("New objects are encrypted with SSE-KMS and the key {key}.", { key: "alias/team-key" })),
    ).toBeInTheDocument();
    await waitFor(() => expect(statusBar().getByText(tr("The default encryption of {bucket} was saved.", { bucket: "demo-empty" }))).toBeInTheDocument());
    expect(within(dialog).getByRole("button", { name: tr("Save") })).toBeDisabled();

    // Switching to SSE-S3 drops the key and the bucket key.
    await user.click(within(dialog).getByRole("radio", { name: new RegExp(tr("SSE-S3")) }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () =>
      expect(await mock.get_bucket_encryption(ref)).toEqual({ algorithm: "AES256", kms_key_id: null, bucket_key_enabled: false }),
    );

    await user.click(await within(dialog).findByRole("button", { name: tr("Remove configuration") }));
    const confirm = await screen.findByRole("dialog", { name: tr("Remove default encryption") });
    await user.click(within(confirm).getByRole("button", { name: tr("Cancel") }));
    expect(await mock.get_bucket_encryption(ref)).not.toBeNull();
    await user.click(within(dialog).getByRole("button", { name: tr("Remove configuration") }));
    await user.click(
      within(await screen.findByRole("dialog", { name: tr("Remove default encryption") })).getByRole("button", { name: tr("Remove configuration") }),
    );
    await waitFor(async () => expect(await mock.get_bucket_encryption(ref)).toBeNull());
  });

  it("does not save a KMS key with a space, and the AWS managed key is an empty key", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: await demoAccount(mock), bucket: "demo-empty" };
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("Encryption"));
    await user.click(await within(dialog).findByRole("radio", { name: new RegExp(tr("SSE-KMS")) }));
    await user.type(within(dialog).getByRole("textbox", { name: new RegExp(tr("KMS key")) }), "alias/a b");
    expect(within(dialog).getByText(tr("The KMS key must be a key ID, key ARN, alias name or alias ARN without spaces."))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Save") })).toBeDisabled();
    await user.clear(within(dialog).getByRole("textbox", { name: new RegExp(tr("KMS key")) }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () =>
      expect(await mock.get_bucket_encryption(ref)).toEqual({ algorithm: "aws:kms", kms_key_id: null, bucket_key_enabled: false }),
    );
  });
});

describe("bucket properties: tags", () => {
  it("adds, edits and removes tags", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: await demoAccount(mock), bucket: "demo-empty" };
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("Tags"));
    await user.click(await within(dialog).findByRole("button", { name: tr("Add tag") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("{label} {n}", { label: tr("Tag key"), n: 1 }) }), "team");
    await user.type(within(dialog).getByRole("textbox", { name: tr("{label} {n}", { label: tr("Tag value"), n: 1 }) }), "data");
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect(await mock.get_bucket_tags(ref)).toEqual({ team: "data" }));
    await waitFor(() => expect(statusBar().getByText(tr("The tags of {bucket} were saved.", { bucket: "demo-empty" }))).toBeInTheDocument());

    // Edit the value.
    const value = await within(dialog).findByRole("textbox", { name: tr("{label} {n}", { label: tr("Tag value"), n: 1 }) });
    expect(value).toHaveValue("data");
    await user.clear(value);
    await user.type(value, "finance");
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect(await mock.get_bucket_tags(ref)).toEqual({ team: "finance" }));

    // Remove the only tag.
    await user.click(await within(dialog).findByRole("button", { name: tr("Remove {label} {n}", { label: tr("Tag key").toLowerCase(), n: 1 }) }));
    await user.click(within(dialog).getByRole("button", { name: tr("Remove all tags") }));
    await waitFor(async () => expect(await mock.get_bucket_tags(ref)).toEqual({}));
  });

  it("reports an invalid tag and a duplicate key without saving", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: await demoAccount(mock), bucket: "demo-empty" };
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("Tags"));
    const add = await within(dialog).findByRole("button", { name: tr("Add tag") });
    await user.click(add);
    await user.type(within(dialog).getByRole("textbox", { name: tr("{label} {n}", { label: tr("Tag key"), n: 1 }) }), "aws:team");
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(tr('Tag keys cannot start with "aws:".'));
    expect(within(dialog).getByRole("button", { name: tr("Save") })).toBeDisabled();
    await user.clear(within(dialog).getByRole("textbox", { name: tr("{label} {n}", { label: tr("Tag key"), n: 1 }) }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("{label} {n}", { label: tr("Tag key"), n: 1 }) }), "team");
    await user.click(add);
    await user.type(within(dialog).getByRole("textbox", { name: tr("{label} {n}", { label: tr("Tag key"), n: 2 }) }), "team");
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(tr('The tag "{key}" appears twice.', { key: "team" }));
    expect(await mock.get_bucket_tags(ref)).toEqual({});
  });
});

describe("bucket properties: ACL", () => {
  it("lists the grants, adds a grant, applies a canned ACL after a confirmation", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: await demoAccount(mock), bucket: "demo-empty" };
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("ACL"));
    const table = await within(dialog).findByRole("table", { name: tr("Grants") });
    expect(table).toHaveTextContent("FULL_CONTROL");
    expect(within(dialog).getByTestId("bucket-acl-state")).toHaveTextContent("private");

    // A grant for a canonical user ID is added and saved with the whole list.
    await user.type(within(dialog).getByRole("textbox", { name: tr("AWS account ID") }), "logs-account-id");
    await user.click(within(dialog).getByRole("combobox", { name: tr("Permission") }));
    await user.click(await within(dialog).findByRole("option", { name: /^WRITE[:：]/ }));
    await user.click(within(dialog).getByRole("button", { name: tr("Add grant") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save grants") }));
    await waitFor(async () => {
      const info = await mock.get_bucket_acl(ref);
      expect(info.acl.grants).toContainEqual({ grantee_type: "canonical_user", grantee: "logs-account-id", permission: "WRITE" });
      expect(info.acl.canned).toBeNull();
    });
    await waitFor(() => expect(within(dialog).getByTestId("bucket-acl-state")).toHaveTextContent(tr("custom grants")));

    // A public canned ACL asks first; cancelling keeps the grants.
    await user.click(within(dialog).getByRole("combobox", { name: tr("Canned ACL") }));
    await user.click(await within(dialog).findByRole("option", { name: tr("Public read (everyone can list the bucket)") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Apply ACL") }));
    const confirm = await screen.findByRole("dialog", { name: tr("Make the bucket public") });
    await user.click(within(confirm).getByRole("button", { name: tr("Cancel") }));
    expect((await mock.get_bucket_acl(ref)).acl.canned).toBeNull();
    await user.click(within(dialog).getByRole("button", { name: tr("Apply ACL") }));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Make the bucket public") })).getByRole("button", { name: tr("Apply ACL") }));
    await waitFor(async () => expect((await mock.get_bucket_acl(ref)).acl.canned).toBe("public-read"));
    await waitFor(() =>
      expect(statusBar().getByText(tr("The ACL of {bucket} is now {acl}.", { bucket: "demo-empty", acl: "public-read" }))).toBeInTheDocument(),
    );
  });

  it("explains a bucket with ACLs disabled and offers no change", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: await demoAccount(mock), bucket: "demo-empty" };
    await mock.put_object_ownership({ ...ref, ownership: "BucketOwnerEnforced" });
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("ACL"));
    expect(await within(dialog).findByText(tr("ACLs are disabled here (Object Ownership: BucketOwnerEnforced), so the grants below cannot be changed. Change the bucket's Object Ownership to let ACLs work, or control access with the bucket policy."))).toBeInTheDocument();
    expect(within(dialog).getByTestId("bucket-acl-state")).toHaveTextContent("BucketOwnerEnforced");
    expect(within(dialog).queryByRole("button", { name: tr("Apply ACL") })).not.toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: tr("Save grants") })).not.toBeInTheDocument();
    await expect(mock.set_bucket_acl({ ...ref, acl: "public-read" })).rejects.toMatchObject({ code: "conflict" });
  });
});
