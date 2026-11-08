import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { BackendError } from "../../api/errors";
import { renderApp, TestPlatform } from "../../test/renderApp";

async function openIam(user: Awaited<ReturnType<typeof renderApp>>["user"]) {
  await screen.findByText("demo-photos");
  fireEvent.contextMenu(document.querySelector(".bb-tree-pane")!);
  const tools = screen.queryByRole("menuitem", { name: /^Tools/ });
  if (tools !== null) {
    await user.click(tools);
  }
  await user.click(await screen.findByRole("menuitem", { name: tr("IAM manager…") }));
  return screen.findByRole("dialog", { name: tr("IAM manager") });
}

describe("IAM manager", () => {
  it("creates a user and a key, shows the secret once and deletes the user after typing its name", async () => {
    const platform = new TestPlatform();
    const { user } = await renderApp({ platform });
    const dialog = await openIam(user);

    await user.type(within(dialog).getByRole("textbox", { name: tr("New user") }), "alice");
    await user.click(within(dialog).getByRole("button", { name: tr("Create") }));
    await within(dialog).findByTestId("iam-user-panel");

    await user.click(within(dialog).getByRole("button", { name: tr("Create access key") }));
    const secret = await within(dialog).findByTestId("iam-secret");
    expect(within(secret).getByText(tr("This is the only time the secret access key is shown. It is not stored anywhere and cannot be recovered; if you lose it, create a new key."))).toBeInTheDocument();
    const value = within(secret).getByTestId("iam-secret-value").textContent;
    expect(value).toBeTruthy();
    expect(value.length).toBeGreaterThan(10);
    await user.click(within(secret).getByRole("button", { name: tr("Copy secret") }));
    expect(platform.copied).toEqual([value]);
    expect(platform.copiedSensitive).toEqual([true]);
    await user.click(within(secret).getByRole("button", { name: tr("I have saved it") }));
    expect(within(dialog).queryByTestId("iam-secret")).not.toBeInTheDocument();
    expect(within(dialog).queryByText(value)).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("Delete user...") }));
    const confirm = await screen.findByRole("dialog", { name: tr("Delete user") });
    const ok = within(confirm).getByRole("button", { name: tr("Delete user") });
    expect(ok).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "alice");
    await waitFor(() => expect(ok).toBeEnabled());
    await user.click(ok);
    await waitFor(() => expect(within(dialog).queryByTestId("iam-user-panel")).not.toBeInTheDocument());
  });

  it("refuses a user delete without the typed name in the mock backend", async () => {
    const { mock } = await renderApp();
    const id = (await mock.list_accounts()).accounts.at(0)?.id ?? "";
    await mock.iam_create_user({ account_id: id, name: "bob", path: null });
    await expect(mock.iam_delete_user({ account_id: id, name: "bob", confirm: "" })).rejects.toBeInstanceOf(BackendError);
    const details = await mock.iam_user_details({ account_id: id, name: "bob" });
    expect(details.user.name).toBe("bob");
    await mock.iam_attach_policy({ account_id: id, user: "bob", policy_arn: "arn:aws:iam::aws:policy/ReadOnlyAccess" });
    expect((await mock.iam_delete_user({ account_id: id, name: "bob", confirm: "bob" })).deleted).toBe(true);
  });

  it("says IAM is not available for an account with a custom endpoint", async () => {
    const { mock } = await renderApp();
    const list = (await mock.list_accounts()).accounts;
    const custom = list.find((account) => account.endpoint !== null);
    if (custom === undefined) {
      return;
    }
    expect(await mock.iam_status({ account_id: custom.id })).toEqual({ supported: false, message: "IAM is not available for this endpoint" });
  });
});
