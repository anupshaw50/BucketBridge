import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { FilterSpec } from "../../api/types";
import { emptyFilterSpec } from "../../lib/filters";
import { MOCK_CREDENTIALS } from "../../mock/world";
import { findObjectRow, openBucket, renderApp, statusBar, TestPlatform } from "../../test/renderApp";

function spec(patch: Partial<FilterSpec>): FilterSpec {
  return { ...emptyFilterSpec(), ...patch };
}

/** Opens an entry of the toolbar's Tools menu. */
async function openTool(user: UserEvent, name: string): Promise<void> {
  await user.click(screen.getByRole("button", { name: "Tools" }));
  await user.click(await screen.findByRole("menuitem", { name }));
}

function visibleNames(): string[] {
  const grid = screen.getByRole("grid", { name: "Objects" });
  return within(grid)
    .queryAllByRole("row")
    .slice(1)
    .map((row) => row.querySelector(".bb-col-name span")?.textContent ?? "");
}

function stepTexts(): string[] {
  return within(screen.getByRole("list", { name: "Diagnostic steps" }))
    .getAllByRole("listitem")
    .map((item) => item.textContent);
}

describe("diagnostics dialog", () => {
  it("runs the diagnostics of the active account and shows every step", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await openTool(user, "Diagnostics...");
    const dialog = await screen.findByRole("dialog", { name: "Diagnostics" });
    expect(within(dialog).getByRole("combobox", { name: "Account" })).toHaveValue("Demo");
    expect(within(dialog).queryByRole("list", { name: "Diagnostic steps" })).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "Run diagnostics" }));
    const steps = await within(dialog).findByRole("list", { name: "Diagnostic steps" });
    expect(within(dialog).getByText("All checks passed.")).toBeInTheDocument();
    const items = within(steps).getAllByRole("listitem");
    expect(items).toHaveLength(6);
    expect(items.map((item) => item.querySelector("p")?.textContent)).toEqual([
      "DNS lookup",
      "TCP connection",
      "Proxy",
      "TLS handshake",
      "Credentials",
      "Bucket region",
    ]);
    const proxy = items[2];
    expect(proxy).toHaveTextContent("Skip");
    expect(proxy).toHaveTextContent("No proxy is configured.");
    expect(proxy).toHaveTextContent("skipped");
    expect(items[0]).toHaveTextContent("Pass");
    expect(items[0]).toHaveTextContent("resolves to");
    expect(items[3]).toHaveTextContent("TLS 1.3 handshake");
    expect(items[3]).toHaveTextContent("Certificate subject: CN=s3.us-east-1.amazonaws.com");
    expect(items[5]).toHaveTextContent("No bucket was named");
    expect(within(dialog).getByRole("button", { name: "Run again" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Copy report" })).toBeInTheDocument();
  });

  it("probes the region of a named bucket and refuses an invalid bucket name", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await openTool(user, "Diagnostics...");
    const dialog = await screen.findByRole("dialog", { name: "Diagnostics" });
    await user.click(within(dialog).getByRole("button", { name: "Run diagnostics" }));
    await within(dialog).findByRole("list", { name: "Diagnostic steps" });

    // A new bucket makes the report out of date: it is dropped.
    const bucket = within(dialog).getByRole("combobox", { name: /Bucket \(optional\)/ });
    await user.type(bucket, "BAD");
    expect(within(dialog).queryByRole("list", { name: "Diagnostic steps" })).not.toBeInTheDocument();
    expect(within(dialog).getByText("Use only lowercase letters, digits, dots (.) and hyphens (-).")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Run diagnostics" })).toBeDisabled();

    await user.clear(bucket);
    await user.type(bucket, "demo-photos");
    await user.click(within(dialog).getByRole("button", { name: "Run diagnostics" }));
    await within(dialog).findByRole("list", { name: "Diagnostic steps" });
    expect(stepTexts()[5]).toContain("The bucket demo-photos is in us-east-1.");

    await user.clear(bucket);
    await user.type(bucket, "no-such-bucket");
    await user.click(within(dialog).getByRole("button", { name: "Run diagnostics" }));
    await within(dialog).findByRole("list", { name: "Diagnostic steps" });
    expect(stepTexts()[5]).toContain("does not exist");
    expect(within(dialog).getByText("1 check failed: Bucket region.")).toBeInTheDocument();
  });

  it("explains a rejected secret and copies the report", async () => {
    const platform = new TestPlatform();
    const { user } = await renderApp({ platform });
    await screen.findByText("demo-photos");
    await openTool(user, "Diagnostics...");
    const dialog = await screen.findByRole("dialog", { name: "Diagnostics" });
    await user.click(within(dialog).getByRole("combobox", { name: "Account" }));
    await user.click(await within(dialog).findByRole("option", { name: "Wrong secret" }));
    expect(within(dialog).getByRole("combobox", { name: "Account" })).toHaveValue("Wrong secret");
    await user.click(within(dialog).getByRole("button", { name: "Run diagnostics" }));
    await within(dialog).findByRole("list", { name: "Diagnostic steps" });
    expect(within(dialog).getByText("1 check failed: Credentials.")).toBeInTheDocument();
    const credentials = within(screen.getByRole("list", { name: "Diagnostic steps" })).getAllByRole("listitem")[4];
    expect(credentials).toHaveTextContent("Fail");
    expect(credentials).toHaveTextContent("The server rejected the signature");
    expect(credentials).toHaveTextContent("bad_signature");

    await user.click(within(dialog).getByRole("button", { name: "Copy report" }));
    expect(platform.copied).toHaveLength(1);
    const text = platform.copied[0] ?? "";
    expect(text).toContain("BucketBridge diagnostics");
    expect(text).toContain("Endpoint: https://s3.us-east-1.amazonaws.com");
    expect(text).toContain("Result: 1 check failed: Credentials.");
    expect(text).toContain("[FAIL] Credentials");
    expect(text).toContain("[PASS] DNS lookup");
    expect(text).not.toContain("not-the-right-secret");
    expect(platform.copiedSensitive).toEqual([false]);
    expect(within(dialog).getByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(statusBar().getByText("Diagnostics report copied to the clipboard.")).toBeInTheDocument();
  });

  it("tests an unsaved account in the add-account wizard without storing it", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: "Account: Demo" }));
    await user.click(await screen.findByRole("menuitem", { name: "Add account..." }));
    const dialog = await screen.findByRole("dialog", { name: "Add account" });
    await user.type(within(dialog).getByRole("textbox", { name: /Label/ }), "Unsaved test");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    await user.type(within(dialog).getByRole("textbox", { name: /Access key ID/ }), MOCK_CREDENTIALS.fresh.accessKeyId);
    const secret = within(dialog).getByLabelText(/Secret access key/);
    await user.type(secret, "typed-the-wrong-secret");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));

    const review = within(dialog).getByRole("group", { name: "Review" });
    expect(within(review).queryByRole("list", { name: "Diagnostic steps" })).not.toBeInTheDocument();
    await user.click(within(review).getByRole("button", { name: "Test connection" }));
    await within(review).findByRole("list", { name: "Diagnostic steps" });
    expect(within(review).getByText("1 check failed: Credentials.")).toBeInTheDocument();
    expect(review).toHaveTextContent("The server rejected the signature");
    expect(review).not.toHaveTextContent("typed-the-wrong-secret");
    // Testing is not adding: the wizard is still open and nothing was stored.
    expect(screen.getByRole("dialog", { name: "Add account" })).toBeInTheDocument();
    expect((await mock.list_accounts()).accounts).toHaveLength(4);

    // Fix the secret and test again.
    await user.click(within(dialog).getByRole("button", { name: "Back" }));
    await user.clear(within(dialog).getByLabelText(/Secret access key/));
    await user.type(within(dialog).getByLabelText(/Secret access key/), MOCK_CREDENTIALS.fresh.secret);
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    const again = within(dialog).getByRole("group", { name: "Review" });
    await user.click(within(again).getByRole("button", { name: "Test connection" }));
    await within(again).findByRole("list", { name: "Diagnostic steps" });
    expect(within(again).getByText("All checks passed.")).toBeInTheDocument();
    expect((await mock.list_accounts()).accounts).toHaveLength(4);

    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).not.toBeInTheDocument());
    expect((await mock.list_accounts()).accounts).toHaveLength(5);
  });

  it("says so when there is no account to probe", async () => {
    const { user } = await renderApp({ mock: { fixture: "empty" } });
    await screen.findByText("Welcome to BucketBridge");
    await openTool(user, "Diagnostics...");
    const dialog = await screen.findByRole("dialog", { name: "Diagnostics" });
    expect(within(dialog).getByText(/Add an account first/)).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: "Run diagnostics" })).not.toBeInTheDocument();
  });
});

describe("uncompleted uploads dialog", () => {
  function tableKeys(): string[] {
    return within(screen.getByRole("table", { name: "Uncompleted uploads" }))
      .getAllByRole("row")
      .slice(1)
      .map((row) => within(row).getAllByRole("cell")[2]?.textContent ?? "");
  }

  it("lists the uploads of the open bucket, selects the old ones and aborts them after a confirmation", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-backups");
    await openTool(user, "Uncompleted uploads...");
    const dialog = await screen.findByRole("dialog", { name: "Uncompleted multipart uploads" });
    await within(dialog).findByRole("table", { name: "Uncompleted uploads" });
    expect(tableKeys()).toEqual(["archive/2018.tar", "archive/2019.tar", "db/2026-10-29.sql.gz"]);
    const ages = within(screen.getByRole("table", { name: "Uncompleted uploads" }))
      .getAllByRole("row")
      .slice(1)
      .map((row) => within(row).getAllByRole("cell")[4]?.textContent);
    expect(ages).toEqual(["100 days", "45 days", "2 days"]);

    expect(within(dialog).getByRole("button", { name: /Abort selected/ })).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: "Select older" }));
    expect(within(dialog).getByText("2 uploads selected of 3")).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "Select archive/2018.tar in demo-backups" })).toBeChecked();
    expect(within(dialog).getByRole("checkbox", { name: "Select archive/2019.tar in demo-backups" })).toBeChecked();
    expect(within(dialog).getByRole("checkbox", { name: "Select db/2026-10-29.sql.gz in demo-backups" })).not.toBeChecked();

    // Declining the confirmation aborts nothing.
    await user.click(within(dialog).getByRole("button", { name: "Abort selected (2)" }));
    let confirm = await screen.findByRole("dialog", { name: "Abort uploads" });
    expect(within(confirm).getByText(/Abort 2 uncompleted uploads\?/)).toBeInTheDocument();
    expect(within(confirm).getByText(/storage is freed/)).toBeInTheDocument();
    await user.click(within(confirm).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Abort uploads" })).not.toBeInTheDocument());
    expect(tableKeys()).toHaveLength(3);
    expect(screen.queryByRole("table", { name: "Abort results" })).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "Abort selected (2)" }));
    confirm = await screen.findByRole("dialog", { name: "Abort uploads" });
    await user.click(within(confirm).getByRole("button", { name: "Abort uploads" }));
    const results = await within(dialog).findByRole("table", { name: "Abort results" });
    const rows = within(results).getAllByRole("row").slice(1);
    expect(rows.map((row) => within(row).getAllByRole("cell").map((cell) => cell.textContent))).toEqual([
      ["demo-backups", "archive/2018.tar", "Aborted"],
      ["demo-backups", "archive/2019.tar", "Aborted"],
    ]);
    expect(tableKeys()).toEqual(["db/2026-10-29.sql.gz"]);
    expect(statusBar().getByText("2 uploads aborted.")).toBeInTheDocument();
    const stored = await mock.list_multipart_uploads({ account_id: (await mock.list_accounts()).accounts[0]?.id ?? "", buckets: ["demo-backups"] });
    expect(stored.uploads.map((row) => row.key)).toEqual(["db/2026-10-29.sql.gz"]);
  });

  it("shows buckets that cannot be listed and uploads that were already gone", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    const demo = (await mock.list_accounts()).accounts[0]?.id ?? "";
    await openTool(user, "Uncompleted uploads...");
    const dialog = await screen.findByRole("dialog", { name: "Uncompleted multipart uploads" });
    // No bucket is open, so none is chosen yet.
    expect(within(dialog).getByRole("button", { name: "List uploads" })).toBeDisabled();
    expect(within(dialog).queryByRole("table", { name: "Uncompleted uploads" })).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "All buckets" }));
    await user.click(within(dialog).getByRole("button", { name: "List uploads" }));
    await within(dialog).findByRole("table", { name: "Uncompleted uploads" });
    expect(tableKeys()).toEqual([
      "archive/2018.tar",
      "archive/2019.tar",
      "db/2026-10-29.sql.gz",
      "app/app-00099.log",
      "photos/2024/raw/IMG_0001.cr2",
      "photos/2025/winter/video.mov",
    ]);
    const alert = within(dialog).getByRole("alert");
    expect(alert).toHaveTextContent("Could not list demo-website");
    expect(alert).toHaveTextContent("Access to demo-website was denied (AccessDenied).");

    // Somebody else aborts one of them first.
    const listed = await mock.list_multipart_uploads({ account_id: demo, buckets: ["demo-logs"] });
    const [gone] = listed.uploads;
    if (!gone) {
      throw new Error("expected an upload in demo-logs");
    }
    await mock.abort_multipart_uploads({ account_id: demo, uploads: [{ bucket: gone.bucket, key: gone.key, upload_id: gone.upload_id }] });
    await user.click(within(dialog).getByRole("checkbox", { name: `Select ${gone.key} in ${gone.bucket}` }));
    await user.click(within(dialog).getByRole("checkbox", { name: "Select photos/2025/winter/video.mov in demo-photos" }));
    await user.click(within(dialog).getByRole("button", { name: "Abort selected (2)" }));
    await user.click(within(await screen.findByRole("dialog", { name: "Abort uploads" })).getByRole("button", { name: "Abort uploads" }));
    const results = await within(dialog).findByRole("table", { name: "Abort results" });
    const texts = within(results)
      .getAllByRole("row")
      .slice(1)
      .map((row) => within(row).getAllByRole("cell").map((cell) => cell.textContent));
    expect(texts).toEqual([
      ["demo-logs", "app/app-00099.log", "The specified upload does not exist (NoSuchUpload)."],
      ["demo-photos", "photos/2025/winter/video.mov", "Aborted"],
    ]);
    // Both are gone from the list; the failed one only counts as a failure in the message.
    expect(tableKeys()).not.toContain("app/app-00099.log");
    expect(tableKeys()).not.toContain("photos/2025/winter/video.mov");
    expect(statusBar().getByText("1 upload aborted.")).toBeInTheDocument();
  });

  it("selects every upload with the header checkbox and validates the number of days", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await openTool(user, "Uncompleted uploads...");
    const dialog = await screen.findByRole("dialog", { name: "Uncompleted multipart uploads" });
    await within(dialog).findByRole("table", { name: "Uncompleted uploads" });
    expect(tableKeys()).toEqual(["photos/2024/raw/IMG_0001.cr2", "photos/2025/winter/video.mov"]);
    await user.click(within(dialog).getByRole("checkbox", { name: "Select all uploads" }));
    expect(within(dialog).getByText("2 uploads selected of 2")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("checkbox", { name: "Select all uploads" }));
    expect(within(dialog).getByText("0 uploads selected of 2")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("checkbox", { name: "Select photos/2025/winter/video.mov in demo-photos" }));
    expect(within(dialog).getByText("1 upload selected of 2")).toBeInTheDocument();

    const days = within(dialog).getByRole("textbox", { name: "Older than (days)" });
    expect(days).toHaveValue("30");
    await user.clear(days);
    expect(within(dialog).getByRole("button", { name: "Select older" })).toBeDisabled();
    await user.type(days, "100");
    await user.click(within(dialog).getByRole("button", { name: "Select older" }));
    expect(within(dialog).getByText("1 upload selected of 2")).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "Select photos/2024/raw/IMG_0001.cr2 in demo-photos" })).toBeChecked();
  });

  it("says when a bucket has no unfinished uploads", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-empty");
    await openTool(user, "Uncompleted uploads...");
    const dialog = await screen.findByRole("dialog", { name: "Uncompleted multipart uploads" });
    expect(await within(dialog).findByText("No uncompleted uploads in the chosen buckets.")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Refresh" })).toBeEnabled();
  });
});

describe("saved filters", () => {
  it("creates, validates, edits and deletes a filter", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await openTool(user, "Saved filters...");
    const dialog = await screen.findByRole("dialog", { name: "Saved filters" });
    expect(within(dialog).getByText("No saved filters yet.")).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "New filter" }));
    expect(within(dialog).getByText("New filter", { selector: "p" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Save filter" })).toBeDisabled();
    await user.type(within(dialog).getByRole("textbox", { name: /Filter name/ }), "Text files");
    // "[[" types one "[".
    await user.type(within(dialog).getByRole("textbox", { name: "Glob pattern" }), "[[a");
    expect(within(dialog).getByText("error parsing glob '[a': unclosed character class; missing ']'")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Save filter" })).toBeDisabled();
    await user.clear(within(dialog).getByRole("textbox", { name: "Glob pattern" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Glob pattern" }), "*.txt");
    expect(within(dialog).queryByText(/error parsing glob/)).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Save filter" }));
    const table = await within(dialog).findByRole("table", { name: "Stored filters" });
    expect(within(table).getByText("Text files")).toBeInTheDocument();
    expect(within(table).getByText("glob *.txt")).toBeInTheDocument();
    expect(statusBar().getByText('Filter "Text files" saved.')).toBeInTheDocument();
    expect((await mock.list_filters()).map((item) => item.name)).toEqual(["Text files"]);

    // The backend refuses a name that is taken, ignoring case; the form stays open with the reason.
    await user.click(within(dialog).getByRole("button", { name: "New filter" }));
    await user.type(within(dialog).getByRole("textbox", { name: /Filter name/ }), "text FILES");
    await user.click(within(dialog).getByRole("button", { name: "Save filter" }));
    expect(await within(dialog).findByText('A saved filter named "text FILES" already exists.')).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));

    // Edit: rename, add a prefix and a size.
    await user.click(within(dialog).getByRole("button", { name: "Edit filter Text files" }));
    expect(within(dialog).getByRole("textbox", { name: "Glob pattern" })).toHaveValue("*.txt");
    const name = within(dialog).getByRole("textbox", { name: /Filter name/ });
    await user.clear(name);
    await user.type(name, "Docs");
    await user.type(within(dialog).getByRole("textbox", { name: "Prefix" }), "documents/");
    await user.type(within(dialog).getByRole("textbox", { name: "Minimum size" }), "2");
    await user.click(within(dialog).getByRole("button", { name: "Save filter" }));
    const edited = await within(dialog).findByRole("table", { name: "Stored filters" });
    expect(within(edited).queryByText("Text files")).not.toBeInTheDocument();
    expect(within(edited).getByText("Docs")).toBeInTheDocument();
    expect(within(edited).getByText('prefix "documents/"; glob *.txt; size at least 2.0 MiB')).toBeInTheDocument();
    expect(await mock.list_filters()).toEqual([
      {
        name: "Docs",
        filter: spec({ prefix: "documents/", glob: "*.txt", min_size: 2 * 1024 * 1024 }),
      },
    ]);

    // Delete after a confirmation.
    await user.click(within(dialog).getByRole("button", { name: "Delete filter Docs" }));
    const confirm = await screen.findByRole("dialog", { name: "Delete filter" });
    await user.click(within(confirm).getByRole("button", { name: "Cancel" }));
    expect(await mock.list_filters()).toHaveLength(1);
    await user.click(within(dialog).getByRole("button", { name: "Delete filter Docs" }));
    await user.click(within(await screen.findByRole("dialog", { name: "Delete filter" })).getByRole("button", { name: "Delete filter" }));
    expect(await within(dialog).findByText("No saved filters yet.")).toBeInTheDocument();
    expect(await mock.list_filters()).toEqual([]);
  });

  it("refuses a reversed size or date range and stores a date range as UTC days", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await openTool(user, "Saved filters...");
    const dialog = await screen.findByRole("dialog", { name: "Saved filters" });
    await user.click(within(dialog).getByRole("button", { name: "New filter" }));
    await user.type(within(dialog).getByRole("textbox", { name: /Filter name/ }), "Ranges");

    await user.type(within(dialog).getByRole("textbox", { name: "Minimum size" }), "5");
    await user.type(within(dialog).getByRole("textbox", { name: "Maximum size" }), "1");
    expect(within(dialog).getByText("min_size (5242880) is larger than max_size (1048576)")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Save filter" })).toBeDisabled();
    await user.clear(within(dialog).getByRole("textbox", { name: "Maximum size" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Maximum size" }), "9");

    fireEvent.change(within(dialog).getByLabelText("Modified on or after"), { target: { value: "2026-02-01" } });
    fireEvent.change(within(dialog).getByLabelText("Modified before"), { target: { value: "2026-01-01" } });
    expect(
      within(dialog).getByText("modified_after must be earlier than modified_before (the interval is empty)"),
    ).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Save filter" })).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText("Modified before"), { target: { value: "2026-07-01" } });
    await user.click(within(dialog).getByRole("button", { name: "Save filter" }));
    await within(dialog).findByRole("table", { name: "Stored filters" });
    expect(await mock.list_filters()).toEqual([
      {
        name: "Ranges",
        filter: spec({
          min_size: 5 * 1024 ** 2,
          max_size: 9 * 1024 ** 2,
          modified_after: "2026-02-01T00:00:00Z",
          modified_before: "2026-07-01T00:00:00Z",
        }),
      },
    ]);
    expect(
      within(dialog).getByText("size 5.0 MiB to 9.0 MiB; modified on or after 2026-02-01; modified before 2026-07-01"),
    ).toBeInTheDocument();

    // Editing shows the stored values again.
    await user.click(within(dialog).getByRole("button", { name: "Edit filter Ranges" }));
    expect(within(dialog).getByRole("textbox", { name: "Minimum size" })).toHaveValue("5");
    expect(within(dialog).getByRole("textbox", { name: "Maximum size" })).toHaveValue("9");
    expect(within(dialog).getByLabelText("Modified on or after")).toHaveValue("2026-02-01");
    expect(within(dialog).getByLabelText("Modified before")).toHaveValue("2026-07-01");
  });

  it("previews how many of the loaded items a filter keeps", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.dblClick(await findObjectRow("documents"));
    await findObjectRow("notes.txt");
    await openTool(user, "Saved filters...");
    const dialog = await screen.findByRole("dialog", { name: "Saved filters" });
    await user.click(within(dialog).getByRole("button", { name: "New filter" }));
    await user.type(within(dialog).getByRole("textbox", { name: "Glob pattern" }), "*.txt");
    expect(within(dialog).getByText("Matches 1 of 3 items loaded in s3://demo-photos/documents/.")).toBeInTheDocument();
    await user.clear(within(dialog).getByRole("textbox", { name: "Glob pattern" }));
    expect(within(dialog).getByText("Matches 3 of 3 items loaded in s3://demo-photos/documents/.")).toBeInTheDocument();
  });

  it("applies a saved filter to the object list, keeps it while browsing and clears it", async () => {
    const { user, mock } = await renderApp();
    await mock.save_filter({ filter: { name: "Text files", filter: spec({ glob: "*.txt" }) }, replaces: null });
    await openBucket(user, "demo-photos");
    expect(screen.getByRole("button", { name: "Apply a saved filter" })).toBeEnabled();
    await user.dblClick(await findObjectRow("documents"));
    await findObjectRow("notes.txt");
    expect(visibleNames()).toEqual(["budget 2026.xlsx", "notes.txt", "report-q3.pdf"]);

    await user.click(screen.getByRole("button", { name: "Apply a saved filter" }));
    await user.click(await screen.findByRole("menuitem", { name: "Text files" }));
    expect(visibleNames()).toEqual(["notes.txt"]);
    expect(screen.getByText("Filter: Text files")).toBeInTheDocument();
    expect(screen.getByText("1 of 3 loaded")).toBeInTheDocument();
    expect(statusBar().getByText('Showing only the items that match "Text files".')).toBeInTheDocument();

    // The menu marks the applied filter.
    await user.click(screen.getByRole("button", { name: "Apply a saved filter" }));
    expect(await screen.findByRole("menuitem", { name: "Text files (applied)" })).toBeInTheDocument();
    await user.keyboard("{Escape}");

    // Folders stay navigable; a filter applies to the folder that is open.
    await user.click(screen.getByRole("button", { name: "Parent folder" }));
    await findObjectRow("documents");
    expect(visibleNames()).toEqual(["archive", "documents", "emoji \u{1F4F7}", "odd", "photos", "space folder"]);
    expect(screen.getByText("Filter: Text files")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Clear saved filter" }));
    await findObjectRow("readme.md");
    expect(screen.queryByText("Filter: Text files")).not.toBeInTheDocument();
    expect(visibleNames()).toHaveLength(7);
  });

  it("says when a filter keeps nothing", async () => {
    const { user, mock } = await renderApp();
    await mock.save_filter({ filter: { name: "Zed", filter: spec({ prefix: "zzz" }) }, replaces: null });
    await openBucket(user, "demo-photos");
    await findObjectRow("readme.md");
    await user.click(screen.getByRole("button", { name: "Apply a saved filter" }));
    await user.click(await screen.findByRole("menuitem", { name: "Zed" }));
    expect(await screen.findByText('None of the loaded items match the saved filter "Zed".')).toBeInTheDocument();
    expect(screen.getByText("0 of 7 loaded")).toBeInTheDocument();
  });

  it("keeps an applied filter in step when it is edited or deleted", async () => {
    const { user, mock } = await renderApp();
    await mock.save_filter({ filter: { name: "Text files", filter: spec({ glob: "*.txt" }) }, replaces: null });
    await openBucket(user, "demo-photos");
    await user.dblClick(await findObjectRow("documents"));
    await findObjectRow("notes.txt");
    await user.click(screen.getByRole("button", { name: "Apply a saved filter" }));
    await user.click(await screen.findByRole("menuitem", { name: "Text files" }));
    expect(visibleNames()).toEqual(["notes.txt"]);

    await user.click(screen.getByRole("button", { name: "Apply a saved filter" }));
    await user.click(await screen.findByRole("menuitem", { name: "Manage filters..." }));
    const dialog = await screen.findByRole("dialog", { name: "Saved filters" });
    expect(within(dialog).getByText("Applied")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Edit filter Text files" }));
    const name = within(dialog).getByRole("textbox", { name: /Filter name/ });
    await user.clear(name);
    await user.type(name, "PDFs");
    const glob = within(dialog).getByRole("textbox", { name: "Glob pattern" });
    await user.clear(glob);
    await user.type(glob, "*.pdf");
    await user.click(within(dialog).getByRole("button", { name: "Save filter" }));
    await within(dialog).findByRole("table", { name: "Stored filters" });
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Saved filters" })).not.toBeInTheDocument());
    expect(screen.getByText("Filter: PDFs")).toBeInTheDocument();
    expect(visibleNames()).toEqual(["report-q3.pdf"]);

    await user.click(screen.getByRole("button", { name: "Apply a saved filter" }));
    await user.click(await screen.findByRole("menuitem", { name: "Manage filters..." }));
    const again = await screen.findByRole("dialog", { name: "Saved filters" });
    await user.click(within(again).getByRole("button", { name: "Delete filter PDFs" }));
    await user.click(within(await screen.findByRole("dialog", { name: "Delete filter" })).getByRole("button", { name: "Delete filter" }));
    await within(again).findByText("No saved filters yet.");
    await user.click(within(again).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Saved filters" })).not.toBeInTheDocument());
    expect(screen.queryByText("Filter: PDFs")).not.toBeInTheDocument();
    expect(visibleNames()).toEqual(["budget 2026.xlsx", "notes.txt", "report-q3.pdf"]);
    expect(await mock.list_filters()).toEqual([]);
  });

  it("offers no filter to apply when none is saved", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: "Apply a saved filter" }));
    expect(await screen.findByRole("menuitem", { name: "No saved filters yet" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Clear saved filter" })).toBeDisabled();
  });
});

describe("export listing", () => {
  it("exports the open folder through the save dialog and offers to show the file", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push("/home/demo/Documents/photos.xlsx");
    const { user, mock } = await renderApp({ platform });
    expect(await screen.findByRole("button", { name: "Export listing" })).toBeDisabled();
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: "Export listing" }));
    const dialog = await screen.findByRole("dialog", { name: "Export listing" });
    expect(within(dialog).getByText("s3://demo-photos/")).toBeInTheDocument();
    expect(within(dialog).getByRole("radio", { name: "CSV" })).toBeChecked();
    expect(within(dialog).getByRole("switch", { name: /Include everything below this folder/ })).not.toBeChecked();
    expect(within(dialog).queryByRole("checkbox", { name: /active filter/ })).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("radio", { name: "Excel workbook" }));
    await user.click(within(dialog).getByRole("button", { name: "Export to file..." }));
    expect(await within(dialog).findByText("Export finished")).toBeInTheDocument();
    expect(dialog).toHaveTextContent("Exported 7 rows");
    expect(dialog).toHaveTextContent("to /home/demo/Documents/photos.xlsx.");
    expect(mock.fs.isFile("/home/demo/Documents/photos.xlsx")).toBe(true);
    expect(platform.saveRequests).toEqual([
      {
        title: "Export listing",
        defaultPath: "demo-photos.xlsx",
        filters: [{ name: "Excel workbook", extensions: ["xlsx"] }],
      },
    ]);
    expect(statusBar().getByText("Exported 7 rows to /home/demo/Documents/photos.xlsx.")).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "Show in folder" }));
    expect(platform.revealed).toEqual(["/home/demo/Documents/photos.xlsx"]);
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Export listing" })).not.toBeInTheDocument());
  });

  it("exports nothing when the save dialog is cancelled", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push(null);
    const { user, mock } = await renderApp({ platform });
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: "Export listing" }));
    const dialog = await screen.findByRole("dialog", { name: "Export listing" });
    await user.click(within(dialog).getByRole("button", { name: "Export to file..." }));
    await waitFor(() => expect(platform.saveRequests).toHaveLength(1));
    expect(platform.saveRequests[0]).toMatchObject({ defaultPath: "demo-photos.csv", filters: [{ name: "CSV", extensions: ["csv"] }] });
    expect(within(dialog).queryByText("Export finished")).not.toBeInTheDocument();
    expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Export to file..." })).toBeEnabled();
    expect(mock.fs.walk("/home/demo").files.filter((file) => file.rel.endsWith(".csv"))).toEqual([]);
    expect(await mock.get_log({ levels: null, text: "Exported", limit: null })).toEqual([]);
  });

  it("includes everything below the folder on request, with a default name for the folder", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push("/home/demo/Documents/all.parquet");
    const { user } = await renderApp({ platform });
    await openBucket(user, "demo-photos");
    await user.dblClick(await findObjectRow("photos"));
    await findObjectRow("2024");
    await user.click(screen.getByRole("button", { name: "Export listing" }));
    const dialog = await screen.findByRole("dialog", { name: "Export listing" });
    expect(within(dialog).getByText("s3://demo-photos/photos/")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("switch", { name: /Include everything below this folder/ }));
    await user.click(within(dialog).getByRole("radio", { name: "Parquet" }));
    await user.click(within(dialog).getByRole("button", { name: "Export to file..." }));
    expect(await within(dialog).findByText("Export finished")).toBeInTheDocument();
    // photos/2024/ (3 files) and photos/2025/ (a marker and snow.jpg).
    expect(dialog).toHaveTextContent("Exported 5 rows");
    expect(platform.saveRequests[0]?.defaultPath).toBe("demo-photos-photos.parquet");
  });

  it("can restrict the export to the entries the active filter keeps", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push("/home/demo/Documents/jpg.csv", "/home/demo/Documents/everything.csv");
    const { user, mock } = await renderApp({ platform });
    await mock.save_filter({ filter: { name: "JPEGs", filter: spec({ glob: "**/*.jpg" }) }, replaces: null });
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: "Apply a saved filter" }));
    await user.click(await screen.findByRole("menuitem", { name: "JPEGs" }));

    await user.click(screen.getByRole("button", { name: "Export listing" }));
    let dialog = await screen.findByRole("dialog", { name: "Export listing" });
    const only = within(dialog).getByRole("checkbox", { name: 'Only entries matching the active filter "JPEGs"' });
    expect(only).toBeChecked();
    await user.click(within(dialog).getByRole("switch", { name: /Include everything below this folder/ }));
    await user.click(within(dialog).getByRole("button", { name: "Export to file..." }));
    expect(await within(dialog).findByText("Export finished")).toBeInTheDocument();
    // Four JPEG files, and the two folder markers that a filter keeps like folders.
    expect(dialog).toHaveTextContent("Exported 6 rows");
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Export listing" })).not.toBeInTheDocument());

    await user.click(screen.getByRole("button", { name: "Export listing" }));
    dialog = await screen.findByRole("dialog", { name: "Export listing" });
    await user.click(within(dialog).getByRole("checkbox", { name: 'Only entries matching the active filter "JPEGs"' }));
    await user.click(within(dialog).getByRole("switch", { name: /Include everything below this folder/ }));
    await user.click(within(dialog).getByRole("button", { name: "Export to file..." }));
    expect(await within(dialog).findByText("Export finished")).toBeInTheDocument();
    expect(dialog).toHaveTextContent("Exported 13 rows");
  });

  it("shows why an export failed and lets the user try again", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push("/no/such/folder/listing.csv", "/home/demo/Documents/listing.csv");
    const { user, mock } = await renderApp({ platform });
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: "Export listing" }));
    const dialog = await screen.findByRole("dialog", { name: "Export listing" });
    await user.click(within(dialog).getByRole("button", { name: "Export to file..." }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("cannot create /no/such/folder/listing.csv");
    expect(mock.fs.isFile("/no/such/folder/listing.csv")).toBe(false);
    await user.click(within(dialog).getByRole("button", { name: "Export to file..." }));
    expect(await within(dialog).findByText("Export finished")).toBeInTheDocument();
    expect(mock.fs.isFile("/home/demo/Documents/listing.csv")).toBe(true);
  });
});
