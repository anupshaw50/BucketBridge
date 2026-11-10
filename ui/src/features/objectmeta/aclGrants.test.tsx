import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import type { MockBackend } from "../../mock/mockBackend";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";

const ALL_USERS = "http://acs.amazonaws.com/groups/global/AllUsers";

async function accountId(mock: MockBackend): Promise<string> {
  return (await mock.list_accounts()).default_account_id ?? "";
}

async function openPermissions(user: UserEvent, name: string): Promise<HTMLElement> {
  await user.click(await findObjectRow(name));
  await user.click(screen.getByRole("button", { name: tr("Properties") }));
  const dialog = await screen.findByRole("dialog", { name: tr("Properties") });
  await user.click(await within(dialog).findByRole("tab", { name: tr("Permissions") }));
  return dialog;
}

async function choose(user: UserEvent, dialog: HTMLElement, select: string, option: string): Promise<void> {
  await user.click(within(dialog).getByRole("combobox", { name: select }));
  await user.click(await within(dialog).findByRole("option", { name: new RegExp(`^${option}`) }));
}

describe("object ACL grants", () => {
  it("adds a grant for an account ID, an e-mail address and a group, then removes one", async () => {
    const { user, mock } = await renderApp();
    const id = await accountId(mock);
    await openBucket(user, "demo-photos");
    const dialog = await openPermissions(user, "readme.md");
    const grants = await within(dialog).findByRole("table", { name: tr("Grants") });
    expect(grants).toHaveTextContent("FULL_CONTROL");

    // An account ID with READ_ACP.
    await user.type(within(dialog).getByRole("textbox", { name: tr("AWS account ID") }), "abc123canonical");
    await choose(user, dialog, tr("Permission"), "READ_ACP[:：]");
    await user.click(within(dialog).getByRole("button", { name: tr("Add grant") }));
    expect(within(dialog).getByRole("table", { name: tr("Grants") })).toHaveTextContent("abc123canonical");

    // An e-mail address with READ.
    await choose(user, dialog, tr("Grantee type"), tr("E-mail address"));
    await user.type(within(dialog).getByRole("textbox", { name: tr("E-mail address") }), "dev@example.com");
    await choose(user, dialog, tr("Permission"), "READ[:：]");
    await user.click(within(dialog).getByRole("button", { name: tr("Add grant") }));

    // The AllUsers group with READ warns that the object becomes public.
    await choose(user, dialog, tr("Grantee type"), tr("Predefined group"));
    await user.click(within(dialog).getByRole("button", { name: tr("Add grant") }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      tr("These grants make the content reachable by people outside your account. The public access block may still stop them."),
    );

    await user.click(within(dialog).getByRole("button", { name: tr("Save grants") }));
    await waitFor(async () => {
      const details = await mock.get_object_details({ account_id: id, bucket: "demo-photos", key: "readme.md", sse_c_key: null });
      expect(details.acl?.grants).toEqual([
        { grantee_type: "canonical_user", grantee: "demo-owner-canonical-id", permission: "FULL_CONTROL" },
        { grantee_type: "canonical_user", grantee: "abc123canonical", permission: "READ_ACP" },
        { grantee_type: "email", grantee: "dev@example.com", permission: "READ" },
        { grantee_type: "group", grantee: ALL_USERS, permission: "READ" },
      ]);
      expect(details.acl?.canned).toBeNull();
    });
    expect(await within(dialog).findByText(tr("custom grants"), { exact: false })).toBeInTheDocument();

    // Removing the extra grants brings back the canned ACL name.
    for (const who of ["abc123canonical", "dev@example.com", tr("Everyone (AllUsers)")]) {
      const permission = who === "abc123canonical" ? "READ_ACP" : "READ";
      await user.click(within(dialog).getByRole("button", { name: tr("Remove the grant {permission} for {who}", { permission, who }) }));
    }
    await user.click(within(dialog).getByRole("button", { name: tr("Save grants") }));
    await waitFor(async () => {
      const details = await mock.get_object_details({ account_id: id, bucket: "demo-photos", key: "readme.md", sse_c_key: null });
      expect(details.acl?.canned).toBe("private");
    });
  });

  it("checks the typed grantee and does not add the same grant twice", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const dialog = await openPermissions(user, "readme.md");
    await within(dialog).findByRole("table", { name: tr("Grants") });
    await user.click(within(dialog).getByRole("button", { name: tr("Add grant") }));
    expect(await within(dialog).findByText(tr("Enter an account ID."))).toBeInTheDocument();
    await choose(user, dialog, tr("Grantee type"), tr("E-mail address"));
    await user.type(within(dialog).getByRole("textbox", { name: tr("E-mail address") }), "nobody");
    await user.click(within(dialog).getByRole("button", { name: tr("Add grant") }));
    expect(await within(dialog).findByText(tr("This is not an e-mail address."))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Save grants") })).toBeDisabled();
    // WRITE does not apply to objects.
    await user.click(within(dialog).getByRole("combobox", { name: tr("Permission") }));
    expect(screen.queryByRole("option", { name: /^WRITE[:：]/ })).not.toBeInTheDocument();
  });

  it("shows the grants of an object in a bucket with ACLs disabled, and explains why they are fixed", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-website");
    const dialog = await openPermissions(user, "index.html");
    expect(await within(dialog).findByText(tr("ACLs are disabled here (Object Ownership: BucketOwnerEnforced), so the grants below cannot be changed. Change the bucket's Object Ownership to let ACLs work, or control access with the bucket policy."))).toBeInTheDocument();
    expect(within(dialog).getByRole("table", { name: tr("Grants") })).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: tr("Save grants") })).not.toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: tr("Add grant") })).not.toBeInTheDocument();
  });
});
