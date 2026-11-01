import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MOCK_CREDENTIALS, MOCK_MFA_CODE } from "../../mock/world";
import { renderApp, statusBar } from "../../test/renderApp";

describe("add-account wizard", () => {
  it("validates each step, reviews, and adds the account", async () => {
    const { user, mock } = await renderApp({ mock: { fixture: "empty" } });
    await user.click(await screen.findByRole("button", { name: "Add account" }));
    const dialog = await screen.findByRole("dialog", { name: "Add account" });

    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText("Enter a label for this account.")).toBeInTheDocument();

    await user.type(within(dialog).getByRole("textbox", { name: /Label/ }), "Work");
    await user.type(within(dialog).getByRole("textbox", { name: /Endpoint/ }), "ftp://storage.example.com");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText("The endpoint must start with https:// or http://.")).toBeInTheDocument();
    await user.clear(within(dialog).getByRole("textbox", { name: /Endpoint/ }));
    await user.click(within(dialog).getByRole("button", { name: "Next" }));

    const credentials = within(dialog).getByRole("group", { name: "Credentials" });
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(credentials).getByText("Enter the access key ID.")).toBeInTheDocument();
    expect(within(credentials).getByText("Enter the secret access key.")).toBeInTheDocument();
    await user.type(within(credentials).getByRole("textbox", { name: /Access key ID/ }), MOCK_CREDENTIALS.fresh.accessKeyId);
    const secret = within(credentials).getByLabelText(/Secret access key/);
    expect(secret).toHaveAttribute("type", "password");
    await user.type(secret, MOCK_CREDENTIALS.fresh.secret);
    await user.click(within(credentials).getByRole("button", { name: "Show or hide the secret access key" }));
    expect(secret).toHaveAttribute("type", "text");

    await user.click(within(dialog).getByRole("button", { name: "Back" }));
    expect(within(dialog).getByRole("textbox", { name: /Label/ })).toHaveValue("Work");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    await user.click(within(dialog).getByRole("button", { name: "Next" }));

    const review = within(dialog).getByRole("group", { name: "Review" });
    expect(within(review).getByText("Work")).toBeInTheDocument();
    expect(within(review).getByText("AWS")).toBeInTheDocument();
    expect(within(review).getByText("Entered (hidden)")).toBeInTheDocument();
    expect(within(review).queryByText(MOCK_CREDENTIALS.fresh.secret)).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).not.toBeInTheDocument());
    expect(await screen.findByText("This account has no buckets yet.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Account: Work" })).toBeInTheDocument();
    const accounts = await mock.list_accounts();
    expect(accounts.accounts.map((account) => account.label)).toEqual(["Work"]);
    expect(statusBar().getByText('Account "Work" added.')).toBeInTheDocument();
  });

  it("rejects a label that is already used", async () => {
    const { user } = await renderApp();
    await user.click(await screen.findByRole("button", { name: "Accounts" }));
    const manager = await screen.findByRole("dialog", { name: "Accounts" });
    await user.click(within(manager).getByRole("button", { name: "Add account..." }));
    const dialog = await screen.findByRole("dialog", { name: "Add account" });
    await user.type(within(dialog).getByRole("textbox", { name: /Label/ }), "DEMO");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText("Another account already uses this label.")).toBeInTheDocument();
  });
});

describe("unlock and MFA prompts", () => {
  it("asks to unlock at startup, rejects a wrong password, then retries the listing", async () => {
    const { user } = await renderApp({ mock: { masterPassword: "correct horse", locked: true } });
    const dialog = await screen.findByRole("dialog", { name: "Unlock secrets" });
    expect(screen.queryByText("demo-photos")).not.toBeInTheDocument();
    await user.type(within(dialog).getByLabelText("Master password"), "wrong one");
    await user.click(within(dialog).getByRole("button", { name: "Unlock" }));
    expect(await within(dialog).findByText("The master password is not correct. Try again.")).toBeInTheDocument();
    await user.type(within(dialog).getByLabelText("Master password"), "correct horse");
    await user.click(within(dialog).getByRole("button", { name: "Unlock" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Unlock secrets" })).not.toBeInTheDocument());
    expect(await screen.findByText("demo-photos")).toBeInTheDocument();
  });

  it("shows the locked error when the prompt is dismissed and unlocks on demand", async () => {
    const { user } = await renderApp({ mock: { masterPassword: "correct horse", locked: true } });
    const dialog = await screen.findByRole("dialog", { name: "Unlock secrets" });
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(/Secrets are locked/)).toBeInTheDocument();
    await user.click(within(alert).getByRole("button", { name: "Unlock" }));
    const again = await screen.findByRole("dialog", { name: "Unlock secrets" });
    await user.type(within(again).getByLabelText("Master password"), "correct horse");
    await user.click(within(again).getByRole("button", { name: "Unlock" }));
    expect(await screen.findByText("demo-photos")).toBeInTheDocument();
  });

  it("locks from the toolbar and prompts again when a command needs the secrets", async () => {
    const { user } = await renderApp({ mock: { masterPassword: "correct horse" } });
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: "Lock secrets" }));
    expect(await screen.findByRole("button", { name: "Unlock secrets" })).toBeInTheDocument();
    await user.click(screen.getByText("demo-photos"));
    const dialog = await screen.findByRole("dialog", { name: "Unlock secrets" });
    await user.type(within(dialog).getByLabelText("Master password"), "correct horse");
    await user.click(within(dialog).getByRole("button", { name: "Unlock" }));
    expect(await screen.findByRole("grid", { name: "Objects" })).toBeInTheDocument();
  });

  it("asks for an MFA code for accounts with an MFA device and retries", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: "Account: Demo" }));
    await user.click(await screen.findByRole("menuitem", { name: /Secure \(MFA\)/ }));
    const dialog = await screen.findByRole("dialog", { name: "Multi-factor authentication" });
    const code = within(dialog).getByRole("textbox", { name: /MFA code/ });
    await user.type(code, "12ab3");
    expect(code).toHaveValue("123");
    await user.clear(code);
    await user.type(code, "000000");
    await user.click(within(dialog).getByRole("button", { name: "Verify" }));
    expect(await within(dialog).findByText(/not accepted/)).toBeInTheDocument();
    await user.type(code, MOCK_MFA_CODE);
    await user.click(within(dialog).getByRole("button", { name: "Verify" }));
    expect(await screen.findByText("secure-vault")).toBeInTheDocument();
  });
});

describe("account manager", () => {
  it("edits, sets the default and removes an account with typed confirmation", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: "Accounts" }));
    const manager = await screen.findByRole("dialog", { name: "Accounts" });

    await user.click(within(manager).getByRole("button", { name: "Edit Wrong secret" }));
    const edit = await screen.findByRole("dialog", { name: "Edit account Wrong secret" });
    const secret = within(edit).getByLabelText(/Secret access key/);
    expect(secret).toHaveValue("");
    await user.type(secret, MOCK_CREDENTIALS.demo.secret);
    await user.click(within(edit).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /Edit account/ })).not.toBeInTheDocument());
    const wrong = (await mock.list_accounts()).accounts.find((account) => account.label === "Wrong secret");
    if (!wrong) throw new Error("account missing");
    await expect(mock.list_buckets({ account_id: wrong.id })).resolves.toHaveLength(5);

    await user.click(within(manager).getByRole("button", { name: "Make Wrong secret the default" }));
    await waitFor(async () => expect((await mock.list_accounts()).default_account_id).toBe(wrong.id));

    await user.click(within(manager).getByRole("button", { name: "Remove Wrong secret" }));
    const confirm = await screen.findByRole("dialog", { name: "Remove account" });
    const remove = within(confirm).getByRole("button", { name: "Remove account" });
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
    await user.click(screen.getByRole("button", { name: "Accounts" }));
    const manager = await screen.findByRole("dialog", { name: "Accounts" });
    await user.click(within(manager).getByRole("button", { name: "Master password..." }));
    const dialog = await screen.findByRole("dialog", { name: "Set a master password" });
    await user.type(within(dialog).getByLabelText("New master password"), "short");
    await user.click(within(dialog).getByRole("button", { name: "Set password" }));
    expect(within(dialog).getByText("Use at least 8 characters.")).toBeInTheDocument();
    expect(within(dialog).getByText("The passwords do not match.")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Generate" }));
    const generated = within(dialog).getByRole("textbox", { name: "Generated password" });
    const value = (generated as HTMLInputElement).value;
    expect(value).toHaveLength(20);
    await user.click(within(dialog).getByRole("button", { name: "Copy generated password" }));
    expect(platform.copied).toContain(value);
    expect(platform.copiedSensitive.at(-1)).toBe(true);
    await user.click(within(dialog).getByRole("button", { name: "Use this password" }));
    expect(within(dialog).getByText(/Strength: (Strong|Very strong)/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Set password" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Set a master password" })).not.toBeInTheDocument());
    expect(await mock.security_status()).toEqual({ master_password_set: true, unlocked: true });
    await mock.lock();
    await expect(mock.unlock({ password: value })).resolves.toBeUndefined();
  });
});
