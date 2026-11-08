import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { MOCK_CREDENTIALS, MOCK_MFA_CODE } from "../../mock/world";
import { renderApp, statusBar } from "../../test/renderApp";

/**
 * Matches a field label exactly, tolerating the required "*" marker and the
 * "显示或隐藏…" toggle button: labels render the marker inline.
 */
function re(text: string): RegExp {
  return new RegExp(`^${text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(\\s*\\*)?$`);
}

describe("add-account wizard", () => {
  it("validates each step, reviews, and adds the account", async () => {
    const { user, mock } = await renderApp({ mock: { fixture: "empty" } });
    await user.click(await screen.findByRole("button", { name: tr("Add account") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Add account") });

    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(within(dialog).getByText(tr("Enter a label for this account."))).toBeInTheDocument();

    await user.type(within(dialog).getByRole("textbox", { name: tr("Label") }), "Work");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Endpoint") }), "ftp://storage.example.com");
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(within(dialog).getByText(tr("The endpoint must start with https:// or http://."))).toBeInTheDocument();
    await user.clear(within(dialog).getByRole("textbox", { name: tr("Endpoint") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));

    const credentials = within(dialog).getByRole("group", { name: tr("Credentials") });
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(within(credentials).getByText(tr("Enter the access key ID."))).toBeInTheDocument();
    expect(within(credentials).getByText(tr("Enter the secret access key."))).toBeInTheDocument();
    await user.type(within(credentials).getByRole("textbox", { name: tr("Access key ID") }), MOCK_CREDENTIALS.fresh.accessKeyId);
    const secret = within(credentials).getByLabelText(re(tr("Secret access key")));
    expect(secret).toHaveAttribute("type", "password");
    await user.type(secret, MOCK_CREDENTIALS.fresh.secret);
    await user.click(within(credentials).getByRole("button", { name: tr("Show or hide the secret access key") }));
    expect(secret).toHaveAttribute("type", "text");

    await user.click(within(dialog).getByRole("button", { name: tr("Back") }));
    expect(within(dialog).getByRole("textbox", { name: tr("Label") })).toHaveValue("Work");
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));

    const review = within(dialog).getByRole("group", { name: tr("Review") });
    expect(within(review).getByText("Work")).toBeInTheDocument();
    expect(within(review).getByText("AWS")).toBeInTheDocument();
    expect(within(review).getByText(tr("Entered (hidden)"))).toBeInTheDocument();
    expect(within(review).queryByText(MOCK_CREDENTIALS.fresh.secret)).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("Add account") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Add account") })).not.toBeInTheDocument());
    expect(await screen.findByText(tr("This account has no buckets yet."))).toBeInTheDocument();
    expect(screen.getByRole("button", { name: tr("Account: {label}", { label: "Work" }) })).toBeInTheDocument();
    const accounts = await mock.list_accounts();
    expect(accounts.accounts.map((account) => account.label)).toEqual(["Work"]);
    expect(statusBar().getByText(tr("Account \"{label}\" added.", { label: "Work" }))).toBeInTheDocument();
  });

  it("rejects a label that is already used", async () => {
    const { user } = await renderApp();
    await user.click(await screen.findByRole("button", { name: tr("Accounts") }));
    const manager = await screen.findByRole("dialog", { name: tr("Accounts") });
    await user.click(within(manager).getByRole("button", { name: tr("Add account...") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Add account") });
    await user.type(within(dialog).getByRole("textbox", { name: tr("Label") }), "DEMO");
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(within(dialog).getByText(tr("Another account already uses this label."))).toBeInTheDocument();
  });
});

describe("unlock and MFA prompts", () => {
  it("asks to unlock at startup, rejects a wrong password, then retries the listing", async () => {
    const { user } = await renderApp({ mock: { masterPassword: "correct horse", locked: true } });
    const dialog = await screen.findByRole("dialog", { name: tr("Unlock secrets") });
    expect(screen.queryByText("demo-photos")).not.toBeInTheDocument();
    await user.type(within(dialog).getByLabelText(tr("Master password")), "wrong one");
    await user.click(within(dialog).getByRole("button", { name: tr("Unlock") }));
    expect(await within(dialog).findByText(tr("The master password is not correct. Try again."))).toBeInTheDocument();
    await user.type(within(dialog).getByLabelText(tr("Master password")), "correct horse");
    await user.click(within(dialog).getByRole("button", { name: tr("Unlock") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Unlock secrets") })).not.toBeInTheDocument());
    expect(await screen.findByText("demo-photos")).toBeInTheDocument();
  });

  it("shows the locked error when the prompt is dismissed and unlocks on demand", async () => {
    const { user } = await renderApp({ mock: { masterPassword: "correct horse", locked: true } });
    const dialog = await screen.findByRole("dialog", { name: tr("Unlock secrets") });
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel") }));
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(/Secrets are locked/)).toBeInTheDocument();
    await user.click(within(alert).getByRole("button", { name: tr("Unlock") }));
    const again = await screen.findByRole("dialog", { name: tr("Unlock secrets") });
    await user.type(within(again).getByLabelText(tr("Master password")), "correct horse");
    await user.click(within(again).getByRole("button", { name: tr("Unlock") }));
    expect(await screen.findByText("demo-photos")).toBeInTheDocument();
  });

  it("locks from the toolbar and prompts again when a command needs the secrets", async () => {
    const { user } = await renderApp({ mock: { masterPassword: "correct horse" } });
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Lock secrets") }));
    expect(await screen.findByRole("button", { name: tr("Unlock secrets") })).toBeInTheDocument();
    await user.click(screen.getByText("demo-photos"));
    const dialog = await screen.findByRole("dialog", { name: tr("Unlock secrets") });
    await user.type(within(dialog).getByLabelText(tr("Master password")), "correct horse");
    await user.click(within(dialog).getByRole("button", { name: tr("Unlock") }));
    expect(await screen.findByRole("grid", { name: tr("Objects") })).toBeInTheDocument();
  });

  it("asks for an MFA code for accounts with an MFA device and retries", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Account: {label}", { label: "Demo" }) }));
    await user.click(await screen.findByRole("menuitem", { name: /Secure \(MFA\)/ }));
    const dialog = await screen.findByRole("dialog", { name: tr("Multi-factor authentication") });
    const code = within(dialog).getByRole("textbox", { name: tr("MFA code") });
    await user.type(code, "12ab3");
    expect(code).toHaveValue("123");
    await user.clear(code);
    await user.type(code, "000000");
    await user.click(within(dialog).getByRole("button", { name: tr("Verify") }));
    expect(await within(dialog).findByText(/not accepted/)).toBeInTheDocument();
    await user.type(code, MOCK_MFA_CODE);
    await user.click(within(dialog).getByRole("button", { name: tr("Verify") }));
    expect(await screen.findByText("secure-vault")).toBeInTheDocument();
  });
});

describe("account manager", () => {
  it("edits, sets the default and removes an account with typed confirmation", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Accounts") }));
    const manager = await screen.findByRole("dialog", { name: tr("Accounts") });

    await user.click(within(manager).getByRole("button", { name: tr("Edit {label}", { label: "Wrong secret" }) }));
    const edit = await screen.findByRole("dialog", { name: tr("Edit account {label}", { label: "Wrong secret" }) });
    const secret = within(edit).getByLabelText(re(tr("Secret access key")));
    expect(secret).toHaveValue("");
    await user.type(secret, MOCK_CREDENTIALS.demo.secret);
    await user.click(within(edit).getByRole("button", { name: tr("Save") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /Edit account/ })).not.toBeInTheDocument());
    const wrong = (await mock.list_accounts()).accounts.find((account) => account.label === "Wrong secret");
    if (!wrong) throw new Error("account missing");
    await expect(mock.list_buckets({ account_id: wrong.id })).resolves.toHaveLength(5);

    await user.click(
      within(manager).getByRole("button", { name: tr("Make {label} the default", { label: "Wrong secret" }) }),
    );
    await waitFor(async () => expect((await mock.list_accounts()).default_account_id).toBe(wrong.id));

    await user.click(within(manager).getByRole("button", { name: tr("Remove {label}", { label: "Wrong secret" }) }));
    const confirm = await screen.findByRole("dialog", { name: tr("Remove account") });
    const remove = within(confirm).getByRole("button", { name: tr("Remove account") });
    expect(remove).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "Wrong secret");
    expect(remove).toBeEnabled();
    await user.click(remove);
    await waitFor(async () =>
      expect((await mock.list_accounts()).accounts.map((account) => account.label)).not.toContain("Wrong secret"),
    );
    expect(within(manager).queryByText("Wrong secret")).not.toBeInTheDocument();
  });

  it("sets a master password with a generated password", async () => {
    const { user, mock, platform } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Accounts") }));
    const manager = await screen.findByRole("dialog", { name: tr("Accounts") });
    await user.click(within(manager).getByRole("button", { name: tr("Master password...") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Set a master password") });
    await user.type(within(dialog).getByLabelText(tr("New master password")), "short");
    await user.click(within(dialog).getByRole("button", { name: tr("Set password") }));
    expect(within(dialog).getByText(tr("Use at least {n} characters.", { n: 8 }))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("The passwords do not match."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Generate") }));
    const generated = within(dialog).getByRole("textbox", { name: tr("Generated password") });
    const value = (generated as HTMLInputElement).value;
    expect(value).toHaveLength(20);
    await user.click(within(dialog).getByRole("button", { name: tr("Copy {label}", { label: tr("Generated password") }) }));
    expect(platform.copied).toContain(value);
    expect(platform.copiedSensitive.at(-1)).toBe(true);
    await user.click(within(dialog).getByRole("button", { name: tr("Use this password") }));
    expect(within(dialog).getByText(new RegExp(`${tr("Strength")}: (${tr("Strong")}|${tr("Very strong")})`))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Set password") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Set a master password") })).not.toBeInTheDocument());
    expect(await mock.security_status()).toEqual({ master_password_set: true, unlocked: true });
    await mock.lock();
    await expect(mock.unlock({ password: value })).resolves.toBeUndefined();
  });
});
