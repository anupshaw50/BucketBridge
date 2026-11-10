import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import type { MockBackend } from "../../mock/mockBackend";
import { renderApp } from "../../test/renderApp";
import { clearCopiedRules } from "./lifecycleModel";

afterEach(clearCopiedRules);

async function demoAccount(mock: MockBackend): Promise<string> {
  const id = (await mock.list_accounts()).default_account_id;
  if (id === null) {
    throw new Error("no default account");
  }
  return id;
}

async function openRules(user: UserEvent, bucket: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
  const row = (await within(tree).findByText(bucket)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${bucket}`);
  }
  fireEvent.contextMenu(row);
  await user.click(await screen.findByRole("menuitem", { name: tr("Properties") }));
  await user.click(within(screen.getByRole("menu", { name: tr("Properties") })).getByRole("menuitem", { name: tr("Lifecycle rules…") }));
  return screen.findByRole("dialog", { name: tr("Bucket rules: {bucket}", { bucket }) });
}

const textbox = (dialog: HTMLElement) => within(dialog).findByRole<HTMLTextAreaElement>("textbox", { name: tr("Lifecycle rules (JSON)") });

const firstId = (editor: HTMLTextAreaElement): string | null => (JSON.parse(editor.value) as { rules: { id: string | null }[] }).rules[0]?.id ?? null;

const RULES = {
  rules: [
    {
      id: "expire-logs",
      status: "enabled",
      filter: { prefix: "logs/", tags: [], object_size_greater_than: null, object_size_less_than: null },
      transitions: [],
      expiration: { days: 30, date: null, expired_object_delete_marker: false },
      noncurrent_version_transitions: [],
      noncurrent_version_expiration: null,
      abort_incomplete_multipart_upload_days: null,
    },
  ],
};

describe("lifecycle rules: the form", () => {
  it("builds rules in the form, shows them as JSON, validates and saves", async () => {
    const { user, mock } = await renderApp();
    const ref = { account_id: await demoAccount(mock), bucket: "demo-empty" };
    const dialog = await openRules(user, "demo-empty");
    await textbox(dialog);
    await user.click(within(dialog).getByRole("radio", { name: tr("Form") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Add rule") }));
    const rule = within(dialog).getByRole("group", { name: tr("Rule {n}", { n: 1 }) });

    await user.type(within(rule).getByRole("textbox", { name: tr("Rule ID") }), "expire-logs");
    await user.type(within(rule).getByRole("textbox", { name: tr("Prefix") }), "logs/");
    // A rule without any action is refused by the validation, which runs by itself in the form view.
    expect(await within(dialog).findByText(/rules\[0\]: has no action/)).toBeInTheDocument();

    await user.type(within(rule).getByRole("textbox", { name: tr("After (days)") }), "30");
    await within(dialog).findByText(tr("The document is valid."));

    // The JSON view shows the same document.
    await user.click(within(dialog).getByRole("radio", { name: "JSON" }));
    expect(JSON.parse((await textbox(dialog)).value)).toEqual(RULES);

    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect((await mock.get_bucket_lifecycle(ref))?.rules[0]).toMatchObject({ id: "expire-logs", expiration: { days: 30 } }));
  });

  it("shows JSON edits in the form and a transition and a tag the form adds in the JSON", async () => {
    const { user, mock } = await renderApp();
    await demoAccount(mock);
    const dialog = await openRules(user, "demo-empty");
    fireEvent.change(await textbox(dialog), { target: { value: JSON.stringify({ rules: [{ id: "a", expiration: { days: 9 } }] }) } });
    await user.click(within(dialog).getByRole("radio", { name: tr("Form") }));
    const rule = within(dialog).getByRole("group", { name: tr("Rule {n}", { n: 1 }) });
    expect(within(rule).getByRole("textbox", { name: tr("Rule ID") })).toHaveValue("a");

    await user.click(within(rule).getByRole("button", { name: tr("Add transition") }));
    await user.click(within(rule).getByRole("button", { name: tr("Add tag") }));
    await user.type(within(rule).getByRole("textbox", { name: tr("Tag key") }), "team");
    await user.click(within(dialog).getByRole("radio", { name: "JSON" }));
    const parsed = JSON.parse((await textbox(dialog)).value) as typeof RULES;
    expect(parsed.rules[0]).toMatchObject({
      id: "a",
      expiration: { days: 9 },
      transitions: [{ days: 30, storage_class: "STANDARD_IA" }],
      filter: { tags: [{ key: "team", value: "" }] },
    });
  });

  it("asks to fix JSON that cannot be shown as a form", async () => {
    const { user, mock } = await renderApp();
    await demoAccount(mock);
    const dialog = await openRules(user, "demo-empty");
    fireEvent.change(await textbox(dialog), { target: { value: "{oops" } });
    await user.click(within(dialog).getByRole("radio", { name: tr("Form") }));
    expect(await within(dialog).findByText(/The JSON cannot be shown as a form|JSON 修正之前/)).toBeInTheDocument();
  });
});

describe("lifecycle rules: copy, paste, import and export", () => {
  it("copies the rules of one bucket and pastes them into another, renaming taken ids", async () => {
    const { user, mock } = await renderApp();
    const account = await demoAccount(mock);
    await mock.put_bucket_lifecycle({ account_id: account, bucket: "demo-empty", config: RULES as never });
    await mock.put_bucket_lifecycle({ account_id: account, bucket: "demo-photos", config: RULES as never });

    const first = await openRules(user, "demo-empty");
    expect(await textbox(first)).toBeInTheDocument();
    const paste = within(first).getByRole("button", { name: tr("Paste rules") });
    expect(paste).toBeDisabled();
    await user.click(within(first).getByRole("button", { name: tr("Copy rules") }));
    await waitFor(() => expect(paste).toBeEnabled());
    await user.click(within(first).getByRole("button", { name: tr("Close") }));

    const second = await openRules(user, "demo-photos");
    await user.click(within(second).getByRole("button", { name: tr("Paste rules") }));
    const text = JSON.parse((await textbox(second)).value) as typeof RULES;
    expect(text.rules.map((rule) => rule.id)).toEqual(["expire-logs", "expire-logs-copy"]);
    // Nothing is saved before Save.
    expect((await mock.get_bucket_lifecycle({ account_id: account, bucket: "demo-photos" }))?.rules).toHaveLength(1);
    await user.click(within(second).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect((await mock.get_bucket_lifecycle({ account_id: account, bucket: "demo-photos" }))?.rules).toHaveLength(2));
  });

  it("exports the document to a file and imports it back", async () => {
    const { user, mock, platform } = await renderApp();
    const account = await demoAccount(mock);
    await mock.put_bucket_lifecycle({ account_id: account, bucket: "demo-empty", config: RULES as never });
    const dialog = await openRules(user, "demo-empty");
    await textbox(dialog);

    platform.saveAnswers.push("/home/demo/Documents/rules.json");
    await user.click(within(dialog).getByRole("button", { name: tr("Export to file...") }));
    await waitFor(() => expect(mock.readRulesFile("/home/demo/Documents/rules.json")).toBeDefined());
    expect(JSON.parse(mock.readRulesFile("/home/demo/Documents/rules.json") ?? "")).toEqual(RULES);
    expect(platform.saveRequests[0]?.defaultPath).toBe("demo-empty-lifecycle.json");

    mock.seedRulesFile("/home/demo/Documents/other.json", JSON.stringify({ rules: [{ id: "imported", expiration: { days: 1 } }] }));
    platform.fileAnswers.push(["/home/demo/Documents/other.json"]);
    await user.click(within(dialog).getByRole("button", { name: tr("Import from file...") }));
    await waitFor(async () => expect(firstId(await textbox(dialog))).toBe("imported"));

    // A file that is not there is an error, and the text stays.
    platform.fileAnswers.push(["/home/demo/Documents/missing.json"]);
    await user.click(within(dialog).getByRole("button", { name: tr("Import from file...") }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("missing.json");
    expect(firstId(await textbox(dialog))).toBe("imported");
  });
});
