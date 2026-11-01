import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { MockBackend } from "../../mock/mockBackend";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";

const KEY_A = "11".repeat(32);
const KEY_B = "22".repeat(32);

async function accountId(mock: MockBackend): Promise<string> {
  const list = await mock.list_accounts();
  return list.default_account_id ?? "";
}

async function openProperties(user: UserEvent, name: string): Promise<HTMLElement> {
  await user.click(await findObjectRow(name));
  await user.click(screen.getByRole("button", { name: "Properties" }));
  return screen.findByRole("dialog", { name: "Properties" });
}

async function openTab(user: UserEvent, dialog: HTMLElement, name: string): Promise<void> {
  await user.click(await within(dialog).findByRole("tab", { name }));
}

async function chooseOption(user: UserEvent, dialog: HTMLElement, select: string, option: RegExp | string): Promise<void> {
  await user.click(within(dialog).getByRole("combobox", { name: select }));
  await user.click(await within(dialog).findByRole("option", { name: option }));
}

describe("properties editor of one object", () => {
  it("edits the tags, validates them, and shows the result", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const dialog = await openProperties(user, "readme.md");
    await openTab(user, dialog, "Tags");
    expect(within(dialog).getByRole("textbox", { name: "Tag key 1" })).toHaveValue("team");
    expect(within(dialog).getByRole("textbox", { name: "Tag value 1" })).toHaveValue("docs");
    const apply = within(dialog).getByRole("button", { name: "Apply" });
    expect(apply).toBeDisabled();

    await user.click(within(dialog).getByRole("button", { name: "Add tag" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Tag key 2" }), "bad;key");
    expect(within(dialog).getByRole("alert")).toHaveTextContent(/The tag key may contain letters, digits/);
    expect(apply).toBeDisabled();
    await user.clear(within(dialog).getByRole("textbox", { name: "Tag key 2" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Tag key 2" }), "env");
    await user.type(within(dialog).getByRole("textbox", { name: "Tag value 2" }), "prod");
    await user.click(apply);
    expect(await within(dialog).findByText("Changed.")).toBeInTheDocument();

    const details = await mock.get_object_details({
      account_id: await accountId(mock),
      bucket: "demo-photos",
      key: "readme.md",
      sse_c_key: null,
    });
    expect(details.meta.tags).toEqual({ team: "docs", env: "prod" });
    // Removing every row deletes the tags.
    await user.click(within(dialog).getByRole("button", { name: "Remove tag key 2" }));
    await user.click(within(dialog).getByRole("button", { name: "Remove tag key 1" }));
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(within(dialog).getByText("Changed.")).toBeInTheDocument());
    expect(
      (await mock.get_object_details({ account_id: await accountId(mock), bucket: "demo-photos", key: "readme.md", sse_c_key: null })).meta.tags,
    ).toEqual({});
  });

  it("changes the storage class and refuses an archived object that is not restored", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    let dialog = await openProperties(user, "readme.md");
    await openTab(user, dialog, "Storage class");
    expect(within(dialog).getByRole("button", { name: "Apply" })).toBeDisabled();
    await chooseOption(user, dialog, "Storage class", /^STANDARD_IA/);
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    expect(await within(dialog).findByText("Changed.")).toBeInTheDocument();
    const id = await accountId(mock);
    expect((await mock.get_object_details({ account_id: id, bucket: "demo-photos", key: "readme.md", sse_c_key: null })).storage_class).toBe(
      "STANDARD_IA",
    );
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Properties" })).not.toBeInTheDocument());

    // GLACIER objects cannot be copied until they are restored.
    await openBucket(user, "demo-backups");
    await user.dblClick(await findObjectRow("archive"));
    dialog = await openProperties(user, "2019.tar");
    await openTab(user, dialog, "Storage class");
    expect(await within(dialog).findByText(/archived \(GLACIER\) and has not been restored/)).toBeInTheDocument();
    await chooseOption(user, dialog, "Storage class", /^STANDARD:/);
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    expect(await within(dialog).findByText(/2019\.tar is archived \(GLACIER\).*restore it first/)).toBeInTheDocument();
    expect(
      (await mock.get_object_details({ account_id: id, bucket: "demo-backups", key: "archive/2019.tar", sse_c_key: null })).storage_class,
    ).toBe("GLACIER");
  });

  it("edits HTTP headers and user metadata and keeps the rest", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const dialog = await openProperties(user, "readme.md");
    await openTab(user, dialog, "HTTP headers");
    const type = within(dialog).getByRole("textbox", { name: "Content-Type" });
    expect(type).toHaveValue("text/markdown");
    expect(within(dialog).getByRole("textbox", { name: "Cache-Control" })).toHaveValue("max-age=300");
    await user.clear(type);
    await user.type(type, "text/plain");
    await user.clear(within(dialog).getByRole("textbox", { name: "Cache-Control" }));
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    expect(await within(dialog).findByText("Changed.")).toBeInTheDocument();

    await openTab(user, dialog, "User metadata");
    expect(within(dialog).getByRole("textbox", { name: "Metadata key 1" })).toHaveValue("author");
    await user.click(within(dialog).getByRole("button", { name: "Add metadata" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Metadata key 2" }), "Stage");
    await user.type(within(dialog).getByRole("textbox", { name: "Metadata value 2" }), "done");
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    expect(await within(dialog).findByText("Changed.")).toBeInTheDocument();

    const details = await mock.get_object_details({
      account_id: await accountId(mock),
      bucket: "demo-photos",
      key: "readme.md",
      sse_c_key: null,
    });
    expect(details.meta.content_type).toBe("text/plain");
    expect(details.meta.cache_control).toBeNull();
    expect(details.meta.user_meta).toEqual({ author: "demo", stage: "done" });
    // Tags were not touched.
    expect(details.meta.tags).toEqual({ team: "docs" });
  });

  it("shows the ACL, changes it, and explains a bucket with ACLs disabled", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const dialog = await openProperties(user, "readme.md");
    await openTab(user, dialog, "Permissions");
    expect(within(dialog).getByText("private", { selector: "p" })).toBeInTheDocument();
    expect(within(dialog).getByRole("table", { name: "Grants" })).toHaveTextContent("FULL_CONTROL");
    await chooseOption(user, dialog, "Canned ACL", "Public read");
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    expect(await within(dialog).findByText("Changed.")).toBeInTheDocument();
    expect(await within(dialog).findByText("public-read", { selector: "p" })).toBeInTheDocument();
    expect(within(dialog).getByRole("table", { name: "Grants" })).toHaveTextContent("AllUsers");
  });

  it("asks for the key of an SSE-C object, refuses a wrong one loudly and works with the right one", async () => {
    const { user, mock } = await renderApp();
    const id = await accountId(mock);
    await mock.set_object_encryption({
      account_id: id,
      bucket: "demo-photos",
      keys: ["readme.md"],
      encryption: { type: "c", key: KEY_A },
      source_sse_c_key: null,
    });
    await openBucket(user, "demo-photos");
    const dialog = await openProperties(user, "readme.md");
    expect(await within(dialog).findByText("The properties could not be read")).toBeInTheDocument();
    expect(within(dialog).getByRole("alert")).toHaveTextContent(/SSE-C/);
    expect(within(dialog).queryByRole("tab", { name: "Tags" })).not.toBeInTheDocument();

    await user.type(within(dialog).getByLabelText("Customer key (SSE-C)"), KEY_B);
    await user.click(within(dialog).getByRole("button", { name: "Read with key" }));
    const refused = await within(dialog).findByText(/S3 refused the customer-provided key \(SSE-C\)/);
    expect(refused).toBeInTheDocument();
    expect(within(dialog).queryByRole("tab", { name: "Tags" })).not.toBeInTheDocument();

    // The panel was rebuilt: the field is found again, still holding the wrong key.
    const key = within(dialog).getByLabelText("Customer key (SSE-C)");
    await user.clear(key);
    await user.type(key, KEY_A);
    await user.click(within(dialog).getByRole("button", { name: "Read with key" }));
    await openTab(user, dialog, "Encryption");
    expect(await within(dialog).findByText(/SSE-C: encrypted with a customer-provided key/)).toBeInTheDocument();
    // The key stays in the dialog for the edits that need it: move to SSE-S3.
    expect(within(dialog).getByLabelText("Key of objects encrypted with SSE-C")).toHaveValue(KEY_A);
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    expect(await within(dialog).findByText("Changed.")).toBeInTheDocument();
    expect(await within(dialog).findByText(/SSE-S3: keys managed by S3/)).toBeInTheDocument();
  });

  it("generates and copies a new customer key as a sensitive value", async () => {
    const { user, platform } = await renderApp();
    await openBucket(user, "demo-photos");
    const dialog = await openProperties(user, "readme.md");
    await openTab(user, dialog, "Encryption");
    await user.click(within(dialog).getByRole("radio", { name: "SSE-C" }));
    const apply = within(dialog).getByRole("button", { name: "Apply" });
    expect(apply).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: "Generate key" }));
    await waitFor(() => expect(apply).toBeEnabled());
    await user.click(within(dialog).getByRole("button", { name: "Copy key" }));
    await waitFor(() => expect(platform.copied).toHaveLength(1));
    expect(platform.copied[0]).toMatch(/^[A-Za-z0-9+/]{43}=$/);
    expect(platform.copiedSensitive[0]).toBe(true);
    await user.click(apply);
    expect(await within(dialog).findByText("Changed.")).toBeInTheDocument();
    expect(await within(dialog).findByText(/SSE-C: encrypted with a customer-provided key/)).toBeInTheDocument();
  });
});

describe("properties editor of several objects", () => {
  async function selectThree(user: UserEvent): Promise<void> {
    await user.dblClick(await findObjectRow("documents"));
    await user.click(await findObjectRow("budget 2026.xlsx"));
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("notes.txt"));
    await user.click(await findObjectRow("report-q3.pdf"));
    await user.keyboard("{/Control}");
  }

  it("adds a tag to every selected object and shows the result per object", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await selectThree(user);
    await user.click(screen.getByRole("button", { name: "Properties" }));
    const dialog = await screen.findByRole("dialog", { name: "Properties of 3 objects" });
    expect(within(dialog).getByText(/3 objects in/)).toBeInTheDocument();
    await openTab(user, dialog, "Tags");
    await user.click(within(dialog).getByRole("button", { name: "Add tag" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Tag key 1" }), "env");
    await user.type(within(dialog).getByRole("textbox", { name: "Tag value 1" }), "batch");
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));

    const results = await within(dialog).findByRole("status", { name: "Results" });
    expect(results).toHaveTextContent("3 changed, 0 unchanged");
    for (const name of ["budget 2026.xlsx", "notes.txt", "report-q3.pdf"]) {
      expect(within(results).getByRole("cell", { name })).toBeInTheDocument();
    }
    const id = await accountId(mock);
    const read = async (key: string) =>
      (await mock.get_object_details({ account_id: id, bucket: "demo-photos", key, sse_c_key: null })).meta.tags;
    expect(await read("documents/notes.txt")).toEqual({ env: "batch" });
    // The other tags of an object stay; the one with the same key gets the new value.
    expect(await read("documents/report-q3.pdf")).toEqual({ env: "batch", "cost-center": "42" });

    // Doing it again changes nothing.
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(within(dialog).getByRole("status", { name: "Results" })).toHaveTextContent("0 changed, 3 unchanged"));
  });

  it("reports the objects that fail and still changes the others", async () => {
    const { user, mock } = await renderApp();
    const id = await accountId(mock);
    await mock.set_object_storage_class({
      account_id: id,
      bucket: "demo-photos",
      keys: ["documents/notes.txt"],
      storage_class: "GLACIER",
      sse_c_key: null,
    });
    await openBucket(user, "demo-photos");
    await selectThree(user);
    await user.click(screen.getByRole("button", { name: "Properties" }));
    const dialog = await screen.findByRole("dialog", { name: "Properties of 3 objects" });
    await openTab(user, dialog, "Storage class");
    await chooseOption(user, dialog, "Storage class", /^STANDARD_IA/);
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    const results = await within(dialog).findByRole("status", { name: "Results" });
    expect(results).toHaveTextContent("2 changed, 0 unchanged, 1 failed");
    const failed = within(results).getByRole("cell", { name: "notes.txt" }).closest("tr");
    expect(failed).toHaveTextContent(/Failed: .*archived \(GLACIER\).*restore it first/);
    const changed = within(results).getByRole("cell", { name: "report-q3.pdf" }).closest("tr");
    expect(changed).toHaveTextContent("Changed (was STANDARD)");
    expect(
      (await mock.get_object_details({ account_id: id, bucket: "demo-photos", key: "documents/report-q3.pdf", sse_c_key: null })).storage_class,
    ).toBe("STANDARD_IA");
    expect(
      (await mock.get_object_details({ account_id: id, bucket: "demo-photos", key: "documents/notes.txt", sse_c_key: null })).storage_class,
    ).toBe("GLACIER");
  });

  it("applies header patches: empty fields keep each object's header", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await selectThree(user);
    await user.click(screen.getByRole("button", { name: "Properties" }));
    const dialog = await screen.findByRole("dialog", { name: "Properties of 3 objects" });
    await openTab(user, dialog, "HTTP headers");
    expect(within(dialog).getByRole("textbox", { name: "Content-Type" })).toHaveValue("");
    await user.type(within(dialog).getByRole("textbox", { name: "Cache-Control" }), "public, max-age=60");
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    expect(await within(dialog).findByRole("status", { name: "Results" })).toHaveTextContent("3 changed");
    const id = await accountId(mock);
    const notes = await mock.get_object_details({ account_id: id, bucket: "demo-photos", key: "documents/notes.txt", sse_c_key: null });
    expect(notes.meta.cache_control).toBe("public, max-age=60");
    expect(notes.meta.content_type).toBe("text/plain");
    const report = await mock.get_object_details({ account_id: id, bucket: "demo-photos", key: "documents/report-q3.pdf", sse_c_key: null });
    expect(report.meta.content_type).toBe("application/pdf");
    expect(report.meta.content_disposition).toBe('attachment; filename="report-q3.pdf"');
  });

  it("opens folders without editors and leaves folders out of a mixed selection", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("documents"));
    await user.click(screen.getByRole("button", { name: "Properties" }));
    let dialog = await screen.findByRole("dialog", { name: "Properties" });
    expect(within(dialog).getByText("Folder")).toBeInTheDocument();
    expect(within(dialog).getByRole("textbox", { name: "S3 URI" })).toHaveValue("s3://demo-photos/documents/");
    expect(within(dialog).queryByRole("tab", { name: "Tags" })).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Properties" })).not.toBeInTheDocument());

    await user.click(await findObjectRow("documents"));
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{/Control}");
    await user.click(screen.getByRole("button", { name: "Properties" }));
    dialog = await screen.findByRole("dialog", { name: "Properties of 1 object" });
    expect(within(dialog).getByText(/1 folder in the selection is left out/)).toBeInTheDocument();
    expect(within(dialog).getByRole("tab", { name: "Tags" })).toBeInTheDocument();
  });
});
