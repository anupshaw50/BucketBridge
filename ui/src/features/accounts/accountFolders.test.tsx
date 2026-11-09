import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { renderApp, statusBar } from "../../test/renderApp";

async function openManager(user: Awaited<ReturnType<typeof renderApp>>["user"]): Promise<HTMLElement> {
  await user.click(await screen.findByRole("button", { name: tr("Accounts") }));
  return screen.findByRole("dialog", { name: tr("Accounts") });
}

function labels(dialog: HTMLElement): string[] {
  const table = within(dialog).getByRole("table", { name: tr("Accounts") });
  return within(table)
    .getAllByRole("row")
    .slice(1)
    .map((row) => row.textContent);
}

describe("account folders and order", () => {
  it("creates a folder, files an account in it, reorders, renames and removes the folder", async () => {
    const { user, mock } = await renderApp();
    const dialog = await openManager(user);
    const before = (await mock.list_accounts()).accounts;
    expect(before.length).toBeGreaterThanOrEqual(2);
    const first = before[0];
    const second = before[1];
    if (first === undefined || second === undefined) {
      throw new Error("the demo has two accounts");
    }

    await user.click(within(dialog).getByRole("button", { name: tr("New folder...") }));
    const naming = await screen.findByRole("dialog", { name: tr("New account folder") });
    await user.type(within(naming).getByRole("textbox", { name: tr("Folder name") }), "Work");
    await user.click(within(naming).getByRole("button", { name: tr("Create") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("New account folder") })).not.toBeInTheDocument());
    expect((await mock.list_accounts()).folders).toEqual([{ name: "Work", account_ids: [] }]);

    // A name that is taken is refused in the form.
    await user.click(within(dialog).getByRole("button", { name: tr("New folder...") }));
    const again = await screen.findByRole("dialog", { name: tr("New account folder") });
    await user.type(within(again).getByRole("textbox", { name: tr("Folder name") }), "work");
    expect(within(again).getByText(tr("A folder with this name already exists."))).toBeInTheDocument();
    expect(within(again).getByRole("button", { name: tr("Create") })).toBeDisabled();
    await user.click(within(again).getByRole("button", { name: tr("Cancel") }));

    // File the second account in the folder: the folder is listed first.
    await user.selectOptions(within(dialog).getByRole("combobox", { name: tr("Folder of {label}", { label: second.label }) }), "Work");
    await waitFor(async () => expect((await mock.list_accounts()).folders[0]?.account_ids).toEqual([second.id]));
    expect(labels(dialog)[1]).toContain(second.label);

    // Moving an account in a list of one stays put; the other accounts reorder.
    await user.click(within(dialog).getByRole("button", { name: tr("Move {label} down", { label: first.label }) }));
    await waitFor(async () => expect((await mock.list_accounts()).accounts[0]?.id).not.toBe(first.id));

    await user.click(within(dialog).getByRole("button", { name: tr("Rename folder {name}", { name: "Work" }) }));
    const renaming = await screen.findByRole("dialog", { name: tr("Rename folder") });
    const field = within(renaming).getByRole("textbox", { name: tr("Folder name") });
    await user.clear(field);
    await user.type(field, "Job");
    await user.click(within(renaming).getByRole("button", { name: tr("Rename") }));
    await waitFor(async () => expect((await mock.list_accounts()).folders[0]?.name).toBe("Job"));

    await user.click(within(dialog).getByRole("button", { name: tr("Remove folder {name}", { name: "Job" }) }));
    const confirm = await screen.findByRole("dialog", { name: tr("Remove folder") });
    await user.click(within(confirm).getByRole("button", { name: tr("Remove folder") }));
    await waitFor(async () => expect((await mock.list_accounts()).folders).toEqual([]));
    expect((await mock.list_accounts()).accounts).toHaveLength(before.length);
  });

  it("groups the account switcher by folder", async () => {
    const { user, mock } = await renderApp();
    const dialog = await openManager(user);
    const target = (await mock.list_accounts()).accounts[0];
    if (target === undefined) {
      throw new Error("no account");
    }
    await user.click(within(dialog).getByRole("button", { name: tr("New folder...") }));
    const naming = await screen.findByRole("dialog", { name: tr("New account folder") });
    await user.type(within(naming).getByRole("textbox", { name: tr("Folder name") }), "Team");
    await user.click(within(naming).getByRole("button", { name: tr("Create") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("New account folder") })).not.toBeInTheDocument());
    await user.selectOptions(within(dialog).getByRole("combobox", { name: tr("Folder of {label}", { label: target.label }) }), "Team");
    await waitFor(async () => expect((await mock.list_accounts()).folders[0]?.account_ids).toEqual([target.id]));
    await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
    await user.click(await screen.findByRole("button", { name: tr("Account: {label}", { label: target.label }) }));
    const menu = await screen.findByRole("menu");
    expect(within(menu).getByText("Team")).toBeInTheDocument();
    expect(within(menu).getByText(tr("Not in a folder"))).toBeInTheDocument();
  });
});

describe("export and import of accounts", () => {
  it("exports without secrets, imports into an empty app and marks accounts that need a secret", async () => {
    const source = await renderApp();
    const dialog = await openManager(source.user);
    source.platform.saveAnswers.push("/home/user/accounts.json");
    await source.user.click(within(dialog).getByRole("button", { name: tr("Export accounts...") }));
    const total = (await source.mock.list_accounts()).accounts.length;
    await waitFor(() =>
      expect(
        statusBar().getByText(tr("Exported {n} accounts to {path}. The file holds no secrets.", { n: total, path: "/home/user/accounts.json" })),
      ).toBeInTheDocument(),
    );
    const text = source.mock.readAccountFile("/home/user/accounts.json");
    expect(text).not.toContain("secret_ref");
    expect(text).not.toContain("session_token");
    expect((JSON.parse(text) as { accounts: unknown[] }).accounts).toHaveLength(total);
    source.unmount();

    const target = await renderApp({ mock: { fixture: "empty" } });
    target.mock.seedAccountFile("/home/user/accounts.json", text);
    const manager = await openManager(target.user);
    target.platform.fileAnswers.push(["/home/user/accounts.json"]);
    await target.user.click(within(manager).getByRole("button", { name: tr("Import accounts...") }));
    const importing = await screen.findByRole("dialog", { name: tr("Import accounts") });
    await target.user.click(within(importing).getByRole("button", { name: tr("Choose file...") }));
    await target.user.click(within(importing).getByRole("button", { name: tr("Import") }));
    expect(await within(importing).findByRole("status")).toBeInTheDocument();
    expect((await target.mock.list_accounts()).accounts.length).toBeGreaterThan(0);
    expect(within(importing).getByText(tr("These accounts need their secret access key"))).toBeInTheDocument();
    await target.user.click(within(importing).getByRole("button", { name: tr("Close") }));
    expect(within(manager).getAllByText(tr("Needs secret")).length).toBeGreaterThan(0);

    // Importing the same file again reports duplicates and changes nothing.
    const before = (await target.mock.list_accounts()).accounts.length;
    target.platform.fileAnswers.push(["/home/user/accounts.json"]);
    await target.user.click(within(manager).getByRole("button", { name: tr("Import accounts...") }));
    const second = await screen.findByRole("dialog", { name: tr("Import accounts") });
    await target.user.click(within(second).getByRole("button", { name: tr("Choose file...") }));
    await target.user.click(within(second).getByRole("button", { name: tr("Import") }));
    expect(await within(second).findByText(tr("No account was imported."))).toBeInTheDocument();
    expect(within(second).getByText(tr("Duplicates"))).toBeInTheDocument();
    expect((await target.mock.list_accounts()).accounts).toHaveLength(before);
  });
});
