import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { MockBackend } from "../../mock/mockBackend";
import { renderApp } from "../../test/renderApp";

async function demoAccount(mock: MockBackend): Promise<string> {
  const id = (await mock.list_accounts()).default_account_id;
  if (id === null) {
    throw new Error("no default account");
  }
  return id;
}

/** Opens a bucket, then a rules dialog from the Properties submenu of its bucket menu. */
async function openRules(user: UserEvent, bucket: string, entry: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: "Buckets and folders" });
  const row = (await within(tree).findByText(bucket)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${bucket}`);
  }
  fireEvent.contextMenu(row);
  await user.click(await screen.findByRole("menuitem", { name: "Properties" }));
  await user.click(within(screen.getByRole("menu", { name: "Properties" })).getByRole("menuitem", { name: entry }));
  return screen.findByRole("dialog", { name: `Bucket rules: ${bucket}` });
}

const CORS = {
  rules: [{ id: "web", allowed_origins: ["https://example.org"], allowed_methods: ["GET"], allowed_headers: [], expose_headers: [], max_age_seconds: null }],
};

describe("bucket rules", () => {
  it("reads, edits, validates, writes and re-reads the CORS rules", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const ref = { account_id: id, bucket: "demo-empty" };
    const dialog = await openRules(user, "demo-empty", "CORS…");
    expect(await within(dialog).findByText(/This bucket has no CORS rules/)).toBeInTheDocument();
    const editor = within(dialog).getByRole<HTMLTextAreaElement>("textbox", { name: "CORS rules (JSON)" });

    // An invalid document lists its problem with the path and is not written.
    fireEvent.change(editor, { target: { value: JSON.stringify({ rules: [{ allowed_origins: [], allowed_methods: ["get"] }] }) } });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(await within(dialog).findByText(/rules\[0\]\.allowed_origins: allow at least one origin/)).toBeInTheDocument();
    expect(within(dialog).getByText(/rules\[0\]\.allowed_methods\[0\]/)).toBeInTheDocument();
    expect(await mock.get_bucket_cors(ref)).toBeNull();

    fireEvent.change(editor, { target: { value: JSON.stringify(CORS) } });
    await user.click(within(dialog).getByRole("button", { name: "Validate" }));
    expect(await within(dialog).findByText("The document is valid.")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(async () => expect(await mock.get_bucket_cors(ref)).toEqual(CORS));
    // Read again: the editor shows the stored document, pretty-printed.
    await waitFor(() => expect(within(dialog).getByRole<HTMLTextAreaElement>("textbox", { name: "CORS rules (JSON)" }).value).toBe(JSON.stringify(CORS, null, 2)));

    await user.click(within(dialog).getByRole("button", { name: "Delete" }));
    const confirm = await screen.findByRole("dialog", { name: "Delete CORS rules" });
    await user.type(within(confirm).getByRole("textbox"), "delete");
    await user.click(within(confirm).getByRole("button", { name: "Delete" }));
    await waitFor(async () => expect(await mock.get_bucket_cors(ref)).toBeNull());
  });

  it("writes lifecycle rules and rejects a transition without days or date", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const ref = { account_id: id, bucket: "demo-empty" };
    const dialog = await openRules(user, "demo-empty", "Lifecycle rules…");
    const editor = await within(dialog).findByRole<HTMLTextAreaElement>("textbox", { name: "Lifecycle rules (JSON)" });
    fireEvent.change(editor, { target: { value: JSON.stringify({ rules: [{ id: "a", transitions: [{ storage_class: "GLACIER" }] }] }) } });
    await user.click(within(dialog).getByRole("button", { name: "Validate" }));
    expect(await within(dialog).findByText(/rules\[0\]\.transitions\[0\]: set days or date/)).toBeInTheDocument();

    const good = { rules: [{ id: "a", expiration: { days: 30 } }] };
    fireEvent.change(editor, { target: { value: JSON.stringify(good) } });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(async () => expect((await mock.get_bucket_lifecycle(ref))?.rules[0]?.id).toBe("a"));
  });

  it("explains that replication needs versioning, then saves once it is enabled", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const ref = { account_id: id, bucket: "demo-empty" };
    const dialog = await openRules(user, "demo-empty", "Replication…");
    const config = {
      role: "arn:aws:iam::123456789012:role/replicate",
      rules: [{ id: "all", priority: 1, status: "enabled", filter: { prefix: null, tags: [] }, destination_bucket: "arn:aws:s3:::backup", storage_class: null, delete_marker_replication: false }],
    };
    const editor = await within(dialog).findByRole<HTMLTextAreaElement>("textbox", { name: "Replication (JSON)" });
    fireEvent.change(editor, { target: { value: JSON.stringify(config) } });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(await within(dialog).findByText(/versioning of 'demo-empty' is not enabled; enable versioning/)).toBeInTheDocument();
    await mock.put_bucket_versioning({ ...ref, status: "enabled" });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(async () => expect((await mock.get_bucket_replication(ref))?.role).toBe(config.role));
  });

  it("manages several Intelligent-Tiering configurations", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const ref = { account_id: id, bucket: "demo-empty" };
    const dialog = await openRules(user, "demo-empty", "Intelligent-Tiering…");
    const editor = await within(dialog).findByRole<HTMLTextAreaElement>("textbox", { name: "Intelligent-Tiering configuration (JSON)" });
    const config = { id: "archive", status: "enabled", filter: { prefix: "cold/", tags: [] }, tierings: [{ access_tier: "ARCHIVE_ACCESS", days: 90 }] };
    fireEvent.change(editor, { target: { value: JSON.stringify({ ...config, tierings: [{ access_tier: "ARCHIVE_ACCESS", days: 1 }] }) } });
    await user.click(within(dialog).getByRole("button", { name: "Validate" }));
    expect(await within(dialog).findByText(/tierings\[0\]\.days: ARCHIVE_ACCESS takes 90 to 730 days/)).toBeInTheDocument();
    fireEvent.change(editor, { target: { value: JSON.stringify(config) } });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(async () => expect(await mock.list_bucket_tiering(ref)).toEqual([config]));
  });
});
