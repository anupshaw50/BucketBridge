import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { FilterSpec } from "../../api/types";
import { tr } from "../../i18n";
import { emptyFilterSpec } from "../../lib/filters";
import { MOCK_CREDENTIALS } from "../../mock/world";
import { findObjectRow, openBucket, renderApp, statusBar, TestPlatform } from "../../test/renderApp";

/** Escapes a string for use in a RegExp. */
const esc = (text: string): string => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** The wildcard slot: pass it as a variable the test does not know. */
const W = "\u0000";

/** Builds a RegExp from a translated sentence whose wildcard variables match anything. */
function sentence(template: string, vars: Record<string, string | number> = {}): RegExp {
  return new RegExp(esc(tr(template, vars)).split(esc(W)).join("[\\s\\S]*?"));
}

function spec(patch: Partial<FilterSpec>): FilterSpec {
  return { ...emptyFilterSpec(), ...patch };
}

/** Opens an entry of the toolbar's Tools menu. */
async function openTool(user: UserEvent, name: string): Promise<void> {
  await user.click(screen.getByRole("button", { name: tr("Tools") }));
  await user.click(await screen.findByRole("menuitem", { name }));
}

function visibleNames(): string[] {
  const grid = screen.getByRole("grid", { name: tr("Objects") });
  return within(grid)
    .queryAllByRole("row")
    .slice(1)
    .map((row) => row.querySelector(".bb-col-name span")?.textContent ?? "");
}

function stepTexts(): string[] {
  return within(screen.getByRole("list", { name: tr("Diagnostic steps") }))
    .getAllByRole("listitem")
    .map((item) => item.textContent);
}

describe("diagnostics dialog", () => {
  it("runs the diagnostics of the active account and shows every step", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await openTool(user, tr("Diagnostics..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Diagnostics") });
    expect(within(dialog).getByRole("combobox", { name: tr("Account") })).toHaveValue("Demo");
    expect(within(dialog).queryByRole("list", { name: tr("Diagnostic steps") })).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("Run diagnostics") }));
    const steps = await within(dialog).findByRole("list", { name: tr("Diagnostic steps") });
    expect(within(dialog).getByText(tr("All checks passed."))).toBeInTheDocument();
    const items = within(steps).getAllByRole("listitem");
    expect(items).toHaveLength(6);
    expect(items.map((item) => item.querySelector("p")?.textContent)).toEqual([
      tr("DNS lookup"),
      tr("TCP connection"),
      tr("Proxy"),
      tr("TLS handshake"),
      tr("Credentials"),
      tr("Bucket region"),
    ]);
    const proxy = items[2];
    expect(proxy).toHaveTextContent(tr("Skip"));
    expect(proxy).toHaveTextContent("No proxy is configured.");
    expect(proxy).toHaveTextContent(tr("skipped"));
    expect(items[0]).toHaveTextContent(tr("Pass"));
    expect(items[0]).toHaveTextContent("resolves to");
    expect(items[3]).toHaveTextContent("TLS 1.3 handshake");
    expect(items[3]).toHaveTextContent("Certificate subject: CN=s3.us-east-1.amazonaws.com");
    expect(items[5]).toHaveTextContent("No bucket was named");
    expect(within(dialog).getByRole("button", { name: tr("Run again") })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Copy report") })).toBeInTheDocument();
  });

  it("probes the region of a named bucket and refuses an invalid bucket name", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await openTool(user, tr("Diagnostics..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Diagnostics") });
    await user.click(within(dialog).getByRole("button", { name: tr("Run diagnostics") }));
    await within(dialog).findByRole("list", { name: tr("Diagnostic steps") });

    // A new bucket makes the report out of date: it is dropped.
    const bucket = within(dialog).getByRole("combobox", { name: tr("Bucket (optional)") });
    await user.type(bucket, "BAD");
    expect(within(dialog).queryByRole("list", { name: tr("Diagnostic steps") })).not.toBeInTheDocument();
    expect(within(dialog).getByText(tr("Use only lowercase letters, digits, dots (.) and hyphens (-)."))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Run diagnostics") })).toBeDisabled();

    await user.clear(bucket);
    await user.type(bucket, "demo-photos");
    await user.click(within(dialog).getByRole("button", { name: tr("Run diagnostics") }));
    await within(dialog).findByRole("list", { name: tr("Diagnostic steps") });
    expect(stepTexts()[5]).toContain("The bucket demo-photos is in us-east-1.");

    await user.clear(bucket);
    await user.type(bucket, "no-such-bucket");
    await user.click(within(dialog).getByRole("button", { name: tr("Run diagnostics") }));
    await within(dialog).findByRole("list", { name: tr("Diagnostic steps") });
    expect(stepTexts()[5]).toContain("does not exist");
    expect(within(dialog).getByText(tr("1 check failed: {names}.", { names: tr("Bucket region") }))).toBeInTheDocument();
  });

  it("explains a rejected secret and copies the report", async () => {
    const platform = new TestPlatform();
    const { user } = await renderApp({ platform });
    await screen.findByText("demo-photos");
    await openTool(user, tr("Diagnostics..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Diagnostics") });
    await user.click(within(dialog).getByRole("combobox", { name: tr("Account") }));
    await user.click(await within(dialog).findByRole("option", { name: "Wrong secret" }));
    expect(within(dialog).getByRole("combobox", { name: tr("Account") })).toHaveValue("Wrong secret");
    await user.click(within(dialog).getByRole("button", { name: tr("Run diagnostics") }));
    await within(dialog).findByRole("list", { name: tr("Diagnostic steps") });
    expect(within(dialog).getByText(tr("1 check failed: {names}.", { names: tr("Credentials") }))).toBeInTheDocument();
    const credentials = within(screen.getByRole("list", { name: tr("Diagnostic steps") })).getAllByRole("listitem")[4];
    expect(credentials).toHaveTextContent(tr("Fail"));
    expect(credentials).toHaveTextContent("The server rejected the signature");
    expect(credentials).toHaveTextContent("bad_signature");

    await user.click(within(dialog).getByRole("button", { name: tr("Copy report") }));
    expect(platform.copied).toHaveLength(1);
    const text = platform.copied[0] ?? "";
    expect(text).toContain(tr("BucketBridge diagnostics"));
    expect(text).toContain(tr("Endpoint: {target}", { target: "https://s3.us-east-1.amazonaws.com" }));
    expect(text).toContain(tr("Result: {verdict}", { verdict: tr("1 check failed: {names}.", { names: tr("Credentials") }) }));
    expect(text).toContain(`[${tr("Fail").toUpperCase()}] ${tr("Credentials")}`);
    expect(text).toContain(`[${tr("Pass").toUpperCase()}] ${tr("DNS lookup")}`);
    expect(text).not.toContain("not-the-right-secret");
    expect(platform.copiedSensitive).toEqual([false]);
    expect(within(dialog).getByRole("button", { name: tr("Copied") })).toBeInTheDocument();
    expect(statusBar().getByText(tr("Diagnostics report copied to the clipboard."))).toBeInTheDocument();
  });

  it("tests an unsaved account in the add-account wizard without storing it", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Account: {label}", { label: "Demo" }) }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Add account...") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Add account") });
    await user.type(within(dialog).getByRole("textbox", { name: tr("Label") }), "Unsaved test");
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Access key ID") }), MOCK_CREDENTIALS.fresh.accessKeyId);
    const secret = within(dialog).getByLabelText(new RegExp(`^${esc(tr("Secret access key"))}`));
    await user.type(secret, "typed-the-wrong-secret");
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));

    const review = within(dialog).getByRole("group", { name: tr("Review") });
    expect(within(review).queryByRole("list", { name: tr("Diagnostic steps") })).not.toBeInTheDocument();
    await user.click(within(review).getByRole("button", { name: tr("Test connection") }));
    await within(review).findByRole("list", { name: tr("Diagnostic steps") });
    expect(within(review).getByText(tr("1 check failed: {names}.", { names: tr("Credentials") }))).toBeInTheDocument();
    expect(review).toHaveTextContent("The server rejected the signature");
    expect(review).not.toHaveTextContent("typed-the-wrong-secret");
    // Testing is not adding: the wizard is still open and nothing was stored.
    expect(screen.getByRole("dialog", { name: tr("Add account") })).toBeInTheDocument();
    expect((await mock.list_accounts()).accounts).toHaveLength(4);

    // Fix the secret and test again.
    await user.click(within(dialog).getByRole("button", { name: tr("Back") }));
    await user.clear(within(dialog).getByLabelText(new RegExp(`^${esc(tr("Secret access key"))}`)));
    await user.type(within(dialog).getByLabelText(new RegExp(`^${esc(tr("Secret access key"))}`)), MOCK_CREDENTIALS.fresh.secret);
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    const again = within(dialog).getByRole("group", { name: tr("Review") });
    await user.click(within(again).getByRole("button", { name: tr("Test connection") }));
    await within(again).findByRole("list", { name: tr("Diagnostic steps") });
    expect(within(again).getByText(tr("All checks passed."))).toBeInTheDocument();
    expect((await mock.list_accounts()).accounts).toHaveLength(4);

    await user.click(within(dialog).getByRole("button", { name: tr("Add account") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Add account") })).not.toBeInTheDocument());
    expect((await mock.list_accounts()).accounts).toHaveLength(5);
  });

  it("says so when there is no account to probe", async () => {
    const { user } = await renderApp({ mock: { fixture: "empty" } });
    await screen.findByText(tr("Welcome to BucketBridge"));
    await openTool(user, tr("Diagnostics..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Diagnostics") });
    expect(within(dialog).getByText(tr("Add an account first: the diagnostics probe the connection of one account."))).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: tr("Run diagnostics") })).not.toBeInTheDocument();
  });
});

describe("uncompleted uploads dialog", () => {
  function tableKeys(): string[] {
    return within(screen.getByRole("table", { name: tr("Uncompleted uploads") }))
      .getAllByRole("row")
      .slice(1)
      .map((row) => within(row).getAllByRole("cell")[2]?.textContent ?? "");
  }

  it("lists the uploads of the open bucket, selects the old ones and aborts them after a confirmation", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-backups");
    await openTool(user, tr("Uncompleted uploads..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Uncompleted multipart uploads") });
    await within(dialog).findByRole("table", { name: tr("Uncompleted uploads") });
    expect(tableKeys()).toEqual(["archive/2018.tar", "archive/2019.tar", "db/2026-10-29.sql.gz"]);
    const ages = within(screen.getByRole("table", { name: tr("Uncompleted uploads") }))
      .getAllByRole("row")
      .slice(1)
      .map((row) => within(row).getAllByRole("cell")[4]?.textContent);
    expect(ages).toEqual([tr("{n} days", { n: 100 }), tr("{n} days", { n: 45 }), tr("{n} days", { n: 2 })]);

    expect(within(dialog).getByRole("button", { name: sentence("Abort selected ({count})", { count: W }) })).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: tr("Select older") }));
    expect(within(dialog).getByText(tr("{count} uploads selected of {total}", { count: 2, total: 3 }))).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: tr("Select {key} in {bucket}", { key: "archive/2018.tar", bucket: "demo-backups" }) })).toBeChecked();
    expect(within(dialog).getByRole("checkbox", { name: tr("Select {key} in {bucket}", { key: "archive/2019.tar", bucket: "demo-backups" }) })).toBeChecked();
    expect(within(dialog).getByRole("checkbox", { name: tr("Select {key} in {bucket}", { key: "db/2026-10-29.sql.gz", bucket: "demo-backups" }) })).not.toBeChecked();

    // Declining the confirmation aborts nothing.
    await user.click(within(dialog).getByRole("button", { name: tr("Abort selected ({count})", { count: 2 }) }));
    let confirm = await screen.findByRole("dialog", { name: tr("Abort uploads") });
    expect(
      within(confirm).getByText(
        sentence(
          "Abort {count} uncompleted uploads? The parts they uploaded so far are deleted and their storage is freed. This cannot be undone.",
          { count: 2 },
        ),
      ),
    ).toBeInTheDocument();
    await user.click(within(confirm).getByRole("button", { name: tr("Cancel") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Abort uploads") })).not.toBeInTheDocument());
    expect(tableKeys()).toHaveLength(3);
    expect(screen.queryByRole("table", { name: tr("Abort results") })).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("Abort selected ({count})", { count: 2 }) }));
    confirm = await screen.findByRole("dialog", { name: tr("Abort uploads") });
    await user.click(within(confirm).getByRole("button", { name: tr("Abort uploads") }));
    const results = await within(dialog).findByRole("table", { name: tr("Abort results") });
    const rows = within(results).getAllByRole("row").slice(1);
    expect(rows.map((row) => within(row).getAllByRole("cell").map((cell) => cell.textContent))).toEqual([
      ["demo-backups", "archive/2018.tar", tr("Aborted")],
      ["demo-backups", "archive/2019.tar", tr("Aborted")],
    ]);
    expect(tableKeys()).toEqual(["db/2026-10-29.sql.gz"]);
    expect(statusBar().getByText(tr("{count} uploads aborted.", { count: 2 }))).toBeInTheDocument();
    const stored = await mock.list_multipart_uploads({ account_id: (await mock.list_accounts()).accounts[0]?.id ?? "", buckets: ["demo-backups"] });
    expect(stored.uploads.map((row) => row.key)).toEqual(["db/2026-10-29.sql.gz"]);
  });

  it("shows buckets that cannot be listed and uploads that were already gone", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    const demo = (await mock.list_accounts()).accounts[0]?.id ?? "";
    await openTool(user, tr("Uncompleted uploads..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Uncompleted multipart uploads") });
    // No bucket is open, so none is chosen yet.
    expect(within(dialog).getByRole("button", { name: tr("List uploads") })).toBeDisabled();
    expect(within(dialog).queryByRole("table", { name: tr("Uncompleted uploads") })).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("All buckets") }));
    await user.click(within(dialog).getByRole("button", { name: tr("List uploads") }));
    await within(dialog).findByRole("table", { name: tr("Uncompleted uploads") });
    expect(tableKeys()).toEqual([
      "archive/2018.tar",
      "archive/2019.tar",
      "db/2026-10-29.sql.gz",
      "app/app-00099.log",
      "photos/2024/raw/IMG_0001.cr2",
      "photos/2025/winter/video.mov",
    ]);
    const alert = within(dialog).getByRole("alert");
    expect(alert).toHaveTextContent(tr("Could not list {bucket}", { bucket: "demo-website" }));
    expect(alert).toHaveTextContent("Access to demo-website was denied (AccessDenied).");

    // Somebody else aborts one of them first.
    const listed = await mock.list_multipart_uploads({ account_id: demo, buckets: ["demo-logs"] });
    const [gone] = listed.uploads;
    if (!gone) {
      throw new Error("expected an upload in demo-logs");
    }
    await mock.abort_multipart_uploads({ account_id: demo, uploads: [{ bucket: gone.bucket, key: gone.key, upload_id: gone.upload_id }] });
    await user.click(within(dialog).getByRole("checkbox", { name: tr("Select {key} in {bucket}", { key: gone.key, bucket: gone.bucket }) }));
    await user.click(
      within(dialog).getByRole("checkbox", { name: tr("Select {key} in {bucket}", { key: "photos/2025/winter/video.mov", bucket: "demo-photos" }) }),
    );
    await user.click(within(dialog).getByRole("button", { name: tr("Abort selected ({count})", { count: 2 }) }));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Abort uploads") })).getByRole("button", { name: tr("Abort uploads") }));
    const results = await within(dialog).findByRole("table", { name: tr("Abort results") });
    const texts = within(results)
      .getAllByRole("row")
      .slice(1)
      .map((row) => within(row).getAllByRole("cell").map((cell) => cell.textContent));
    expect(texts).toEqual([
      ["demo-logs", "app/app-00099.log", "The specified upload does not exist (NoSuchUpload)."],
      ["demo-photos", "photos/2025/winter/video.mov", tr("Aborted")],
    ]);
    // Both are gone from the list; the failed one only counts as a failure in the message.
    expect(tableKeys()).not.toContain("app/app-00099.log");
    expect(tableKeys()).not.toContain("photos/2025/winter/video.mov");
    expect(statusBar().getByText(tr("1 upload aborted."))).toBeInTheDocument();
  });

  it("selects every upload with the header checkbox and validates the number of days", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await openTool(user, tr("Uncompleted uploads..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Uncompleted multipart uploads") });
    await within(dialog).findByRole("table", { name: tr("Uncompleted uploads") });
    expect(tableKeys()).toEqual(["photos/2024/raw/IMG_0001.cr2", "photos/2025/winter/video.mov"]);
    await user.click(within(dialog).getByRole("checkbox", { name: tr("Select all uploads") }));
    expect(within(dialog).getByText(tr("{count} uploads selected of {total}", { count: 2, total: 2 }))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("checkbox", { name: tr("Select all uploads") }));
    expect(within(dialog).getByText(tr("{count} uploads selected of {total}", { count: 0, total: 2 }))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("checkbox", { name: tr("Select {key} in {bucket}", { key: "photos/2025/winter/video.mov", bucket: "demo-photos" }) }));
    expect(within(dialog).getByText(tr("1 upload selected of {count}", { count: 2 }))).toBeInTheDocument();

    const days = within(dialog).getByRole("textbox", { name: tr("Older than (days)") });
    expect(days).toHaveValue("30");
    await user.clear(days);
    expect(within(dialog).getByRole("button", { name: tr("Select older") })).toBeDisabled();
    await user.type(days, "100");
    await user.click(within(dialog).getByRole("button", { name: tr("Select older") }));
    expect(within(dialog).getByText(tr("1 upload selected of {count}", { count: 2 }))).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: tr("Select {key} in {bucket}", { key: "photos/2024/raw/IMG_0001.cr2", bucket: "demo-photos" }) })).toBeChecked();
  });

  it("says when a bucket has no unfinished uploads", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-empty");
    await openTool(user, tr("Uncompleted uploads..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Uncompleted multipart uploads") });
    expect(await within(dialog).findByText(tr("No uncompleted uploads in the chosen buckets."))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Refresh") })).toBeEnabled();
  });
});

describe("saved filters", () => {
  it("creates, validates, edits and deletes a filter", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await openTool(user, tr("Saved filters..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Saved filters") });
    expect(within(dialog).getByText(tr("No saved filters yet."))).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("New filter") }));
    expect(within(dialog).getByText(tr("New filter"), { selector: "p" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Save filter") })).toBeDisabled();
    await user.type(within(dialog).getByRole("textbox", { name: tr("Filter name") }), "Text files");
    // "[[" types one "[".
    await user.type(within(dialog).getByRole("textbox", { name: tr("Glob pattern") }), "[[a");
    expect(within(dialog).getByText("error parsing glob '[a': unclosed character class; missing ']'")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Save filter") })).toBeDisabled();
    await user.clear(within(dialog).getByRole("textbox", { name: tr("Glob pattern") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Glob pattern") }), "*.txt");
    expect(within(dialog).queryByText(/error parsing glob/)).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Save filter") }));
    const table = await within(dialog).findByRole("table", { name: tr("Stored filters") });
    expect(within(table).getByText("Text files")).toBeInTheDocument();
    expect(within(table).getByText(tr("glob {glob}", { glob: "*.txt" }))).toBeInTheDocument();
    expect(statusBar().getByText(tr("Filter \"{name}\" saved.", { name: "Text files" }))).toBeInTheDocument();
    expect((await mock.list_filters()).map((item) => item.name)).toEqual(["Text files"]);

    // The backend refuses a name that is taken, ignoring case; the form stays open with the reason.
    await user.click(within(dialog).getByRole("button", { name: tr("New filter") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Filter name") }), "text FILES");
    await user.click(within(dialog).getByRole("button", { name: tr("Save filter") }));
    expect(await within(dialog).findByText('A saved filter named "text FILES" already exists.')).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel") }));

    // Edit: rename, add a prefix and a size.
    await user.click(within(dialog).getByRole("button", { name: tr("Edit filter {name}", { name: "Text files" }) }));
    expect(within(dialog).getByRole("textbox", { name: tr("Glob pattern") })).toHaveValue("*.txt");
    const name = within(dialog).getByRole("textbox", { name: tr("Filter name") });
    await user.clear(name);
    await user.type(name, "Docs");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Prefix") }), "documents/");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Minimum size") }), "2");
    await user.click(within(dialog).getByRole("button", { name: tr("Save filter") }));
    const edited = await within(dialog).findByRole("table", { name: tr("Stored filters") });
    expect(within(edited).queryByText("Text files")).not.toBeInTheDocument();
    expect(within(edited).getByText("Docs")).toBeInTheDocument();
    expect(within(edited).getByText(new RegExp(esc(tr("size at least {min}", { min: "2.0 MiB" }))))).toBeInTheDocument();
    expect(await mock.list_filters()).toEqual([
      {
        name: "Docs",
        filter: spec({ prefix: "documents/", glob: "*.txt", min_size: 2 * 1024 * 1024 }),
      },
    ]);

    // Delete after a confirmation.
    await user.click(within(dialog).getByRole("button", { name: tr("Delete filter {name}", { name: "Docs" }) }));
    const confirm = await screen.findByRole("dialog", { name: tr("Delete filter") });
    await user.click(within(confirm).getByRole("button", { name: tr("Cancel") }));
    expect(await mock.list_filters()).toHaveLength(1);
    await user.click(within(dialog).getByRole("button", { name: tr("Delete filter {name}", { name: "Docs" }) }));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Delete filter") })).getByRole("button", { name: tr("Delete filter") }));
    expect(await within(dialog).findByText(tr("No saved filters yet."))).toBeInTheDocument();
    expect(await mock.list_filters()).toEqual([]);
  });

  it("refuses a reversed size or date range and stores a date range as UTC days", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await openTool(user, tr("Saved filters..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Saved filters") });
    await user.click(within(dialog).getByRole("button", { name: tr("New filter") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Filter name") }), "Ranges");

    await user.type(within(dialog).getByRole("textbox", { name: tr("Minimum size") }), "5");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Maximum size") }), "1");
    expect(within(dialog).getByText("min_size (5242880) is larger than max_size (1048576)")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Save filter") })).toBeDisabled();
    await user.clear(within(dialog).getByRole("textbox", { name: tr("Maximum size") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Maximum size") }), "9");

    fireEvent.change(within(dialog).getByLabelText(tr("Modified on or after")), { target: { value: "2026-02-01" } });
    fireEvent.change(within(dialog).getByLabelText(tr("Modified before")), { target: { value: "2026-01-01" } });
    expect(
      within(dialog).getByText("modified_after must be earlier than modified_before (the interval is empty)"),
    ).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Save filter") })).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText(tr("Modified before")), { target: { value: "2026-07-01" } });
    await user.click(within(dialog).getByRole("button", { name: tr("Save filter") }));
    await within(dialog).findByRole("table", { name: tr("Stored filters") });
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
      within(dialog).getByText(`${tr("size {min} to {max}", { min: "5.0 MiB", max: "9.0 MiB" })}; ${tr("modified on or after {time}", { time: "2026-02-01" })}; ${tr("modified before {time}", { time: "2026-07-01" })}`),
    ).toBeInTheDocument();

    // Editing shows the stored values again.
    await user.click(within(dialog).getByRole("button", { name: tr("Edit filter {name}", { name: "Ranges" }) }));
    expect(within(dialog).getByRole("textbox", { name: tr("Minimum size") })).toHaveValue("5");
    expect(within(dialog).getByRole("textbox", { name: tr("Maximum size") })).toHaveValue("9");
    expect(within(dialog).getByLabelText(tr("Modified on or after"))).toHaveValue("2026-02-01");
    expect(within(dialog).getByLabelText(tr("Modified before"))).toHaveValue("2026-07-01");
  });

  it("previews how many of the loaded items a filter keeps", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.dblClick(await findObjectRow("documents"));
    await findObjectRow("notes.txt");
    await openTool(user, tr("Saved filters..."));
    const dialog = await screen.findByRole("dialog", { name: tr("Saved filters") });
    await user.click(within(dialog).getByRole("button", { name: tr("New filter") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Glob pattern") }), "*.txt");
    expect(
      within(dialog).getByText(tr("Matches {kept} of {total} items loaded in s3://{bucket}/{prefix}.", { kept: 1, total: 3, bucket: "demo-photos", prefix: "documents/" })),
    ).toBeInTheDocument();
    await user.clear(within(dialog).getByRole("textbox", { name: tr("Glob pattern") }));
    expect(
      within(dialog).getByText(tr("Matches {kept} of {total} items loaded in s3://{bucket}/{prefix}.", { kept: 3, total: 3, bucket: "demo-photos", prefix: "documents/" })),
    ).toBeInTheDocument();
  });

  it("applies a saved filter to the object list, keeps it while browsing and clears it", async () => {
    const { user, mock } = await renderApp();
    await mock.save_filter({ filter: { name: "Text files", filter: spec({ glob: "*.txt" }) }, replaces: null });
    await openBucket(user, "demo-photos");
    expect(screen.getByRole("button", { name: tr("Apply a saved filter") })).toBeEnabled();
    await user.dblClick(await findObjectRow("documents"));
    await findObjectRow("notes.txt");
    expect(visibleNames()).toEqual(["budget 2026.xlsx", "notes.txt", "report-q3.pdf"]);

    await user.click(screen.getByRole("button", { name: tr("Apply a saved filter") }));
    await user.click(await screen.findByRole("menuitem", { name: "Text files" }));
    expect(visibleNames()).toEqual(["notes.txt"]);
    expect(screen.getByText(tr("Filter: {name}", { name: "Text files" }))).toBeInTheDocument();
    expect(screen.getByText(tr("Loaded {shown} of {loaded}", { shown: "1", loaded: "3" }))).toBeInTheDocument();
    expect(statusBar().getByText(tr("Showing only the items that match \"{name}\".", { name: "Text files" }))).toBeInTheDocument();

    // The menu marks the applied filter.
    await user.click(screen.getByRole("button", { name: tr("Apply a saved filter") }));
    expect(await screen.findByRole("menuitem", { name: tr("{name} (applied)", { name: "Text files" }) })).toBeInTheDocument();
    await user.keyboard("{Escape}");

    // Folders stay navigable; a filter applies to the folder that is open.
    await user.click(screen.getByRole("button", { name: tr("Parent folder") }));
    await findObjectRow("documents");
    expect(visibleNames()).toEqual(["archive", "documents", "emoji \u{1F4F7}", "odd", "photos", "space folder"]);
    expect(screen.getByText(tr("Filter: {name}", { name: "Text files" }))).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: tr("Clear saved filter") }));
    await findObjectRow("readme.md");
    expect(screen.queryByText(tr("Filter: {name}", { name: "Text files" }))).not.toBeInTheDocument();
    expect(visibleNames()).toHaveLength(7);
  });

  it("says when a filter keeps nothing", async () => {
    const { user, mock } = await renderApp();
    await mock.save_filter({ filter: { name: "Zed", filter: spec({ prefix: "zzz" }) }, replaces: null });
    await openBucket(user, "demo-photos");
    await findObjectRow("readme.md");
    await user.click(screen.getByRole("button", { name: tr("Apply a saved filter") }));
    await user.click(await screen.findByRole("menuitem", { name: "Zed" }));
    expect(await screen.findByText(tr("None of the loaded items match the saved filter \"{name}\".", { name: "Zed" }))).toBeInTheDocument();
    expect(screen.getByText(tr("Loaded {shown} of {loaded}", { shown: "0", loaded: "7" }))).toBeInTheDocument();
  });

  it("keeps an applied filter in step when it is edited or deleted", async () => {
    const { user, mock } = await renderApp();
    await mock.save_filter({ filter: { name: "Text files", filter: spec({ glob: "*.txt" }) }, replaces: null });
    await openBucket(user, "demo-photos");
    await user.dblClick(await findObjectRow("documents"));
    await findObjectRow("notes.txt");
    await user.click(screen.getByRole("button", { name: tr("Apply a saved filter") }));
    await user.click(await screen.findByRole("menuitem", { name: "Text files" }));
    expect(visibleNames()).toEqual(["notes.txt"]);

    await user.click(screen.getByRole("button", { name: tr("Apply a saved filter") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Manage filters...") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Saved filters") });
    expect(within(dialog).getByText(tr("Applied"))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Edit filter {name}", { name: "Text files" }) }));
    const name = within(dialog).getByRole("textbox", { name: tr("Filter name") });
    await user.clear(name);
    await user.type(name, "PDFs");
    const glob = within(dialog).getByRole("textbox", { name: tr("Glob pattern") });
    await user.clear(glob);
    await user.type(glob, "*.pdf");
    await user.click(within(dialog).getByRole("button", { name: tr("Save filter") }));
    await within(dialog).findByRole("table", { name: tr("Stored filters") });
    await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Saved filters") })).not.toBeInTheDocument());
    expect(screen.getByText(tr("Filter: {name}", { name: "PDFs" }))).toBeInTheDocument();
    expect(visibleNames()).toEqual(["report-q3.pdf"]);

    await user.click(screen.getByRole("button", { name: tr("Apply a saved filter") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Manage filters...") }));
    const again = await screen.findByRole("dialog", { name: tr("Saved filters") });
    await user.click(within(again).getByRole("button", { name: tr("Delete filter {name}", { name: "PDFs" }) }));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Delete filter") })).getByRole("button", { name: tr("Delete filter") }));
    await within(again).findByText(tr("No saved filters yet."));
    await user.click(within(again).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Saved filters") })).not.toBeInTheDocument());
    expect(screen.queryByText(tr("Filter: {name}", { name: "PDFs" }))).not.toBeInTheDocument();
    expect(visibleNames()).toEqual(["budget 2026.xlsx", "notes.txt", "report-q3.pdf"]);
    expect(await mock.list_filters()).toEqual([]);
  });

  it("offers no filter to apply when none is saved", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Apply a saved filter") }));
    expect(await screen.findByRole("menuitem", { name: tr("No saved filters yet") })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: tr("Clear saved filter") })).toBeDisabled();
  });
});

describe("export listing", () => {
  it("exports the open folder through the save dialog and offers to show the file", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push("/home/demo/Documents/photos.xlsx");
    const { user, mock } = await renderApp({ platform });
    expect(await screen.findByRole("button", { name: tr("Export listing") })).toBeDisabled();
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Export listing") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Export listing") });
    expect(within(dialog).getByText("s3://demo-photos/")).toBeInTheDocument();
    expect(within(dialog).getByRole("radio", { name: "CSV" })).toBeChecked();
    expect(within(dialog).getByRole("switch", { name: new RegExp(esc(tr("Include everything below this folder"))) })).not.toBeChecked();
    expect(within(dialog).queryByRole("checkbox", { name: /active filter/ })).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("radio", { name: tr("Excel workbook") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Export to file...") }));
    expect(await within(dialog).findByText(tr("Export finished"))).toBeInTheDocument();
    expect(dialog).toHaveTextContent(sentence("Exported {count} rows ({size}) to {path}.", { count: 7, size: W, path: W }));
    expect(dialog).toHaveTextContent("/home/demo/Documents/photos.xlsx");
    expect(mock.fs.isFile("/home/demo/Documents/photos.xlsx")).toBe(true);
    expect(platform.saveRequests).toEqual([
      {
        title: tr("Export listing"),
        defaultPath: "demo-photos.xlsx",
        filters: [{ name: tr("Excel workbook"), extensions: ["xlsx"] }],
      },
    ]);
    expect(statusBar().getByText(tr("Exported {count} rows to {path}.", { count: 7, path: "/home/demo/Documents/photos.xlsx" }))).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: tr("Show in folder") }));
    expect(platform.revealed).toEqual(["/home/demo/Documents/photos.xlsx"]);
    await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Export listing") })).not.toBeInTheDocument());
  });

  it("exports nothing when the save dialog is cancelled", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push(null);
    const { user, mock } = await renderApp({ platform });
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Export listing") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Export listing") });
    await user.click(within(dialog).getByRole("button", { name: tr("Export to file...") }));
    await waitFor(() => expect(platform.saveRequests).toHaveLength(1));
    expect(platform.saveRequests[0]).toMatchObject({ defaultPath: "demo-photos.csv", filters: [{ name: "CSV", extensions: ["csv"] }] });
    expect(within(dialog).queryByText(tr("Export finished"))).not.toBeInTheDocument();
    expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Export to file...") })).toBeEnabled();
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
    await user.click(screen.getByRole("button", { name: tr("Export listing") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Export listing") });
    expect(within(dialog).getByText("s3://demo-photos/photos/")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("switch", { name: new RegExp(esc(tr("Include everything below this folder"))) }));
    await user.click(within(dialog).getByRole("radio", { name: "Parquet" }));
    await user.click(within(dialog).getByRole("button", { name: tr("Export to file...") }));
    expect(await within(dialog).findByText(tr("Export finished"))).toBeInTheDocument();
    // photos/2024/ (3 files) and photos/2025/ (a marker and snow.jpg).
    expect(dialog).toHaveTextContent(sentence("Exported {count} rows ({size}) to {path}.", { count: 5, size: W, path: W }));
    expect(platform.saveRequests[0]?.defaultPath).toBe("demo-photos-photos.parquet");
  });

  it("can restrict the export to the entries the active filter keeps", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push("/home/demo/Documents/jpg.csv", "/home/demo/Documents/everything.csv");
    const { user, mock } = await renderApp({ platform });
    await mock.save_filter({ filter: { name: "JPEGs", filter: spec({ glob: "**/*.jpg" }) }, replaces: null });
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Apply a saved filter") }));
    await user.click(await screen.findByRole("menuitem", { name: "JPEGs" }));

    await user.click(screen.getByRole("button", { name: tr("Export listing") }));
    let dialog = await screen.findByRole("dialog", { name: tr("Export listing") });
    const only = within(dialog).getByRole("checkbox", { name: tr("Only entries matching the active filter \"{name}\"", { name: "JPEGs" }) });
    expect(only).toBeChecked();
    await user.click(within(dialog).getByRole("switch", { name: new RegExp(esc(tr("Include everything below this folder"))) }));
    await user.click(within(dialog).getByRole("button", { name: tr("Export to file...") }));
    expect(await within(dialog).findByText(tr("Export finished"))).toBeInTheDocument();
    // Four JPEG files, and the two folder markers that a filter keeps like folders.
    expect(dialog).toHaveTextContent(sentence("Exported {count} rows ({size}) to {path}.", { count: 6, size: W, path: W }));
    await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Export listing") })).not.toBeInTheDocument());

    await user.click(screen.getByRole("button", { name: tr("Export listing") }));
    dialog = await screen.findByRole("dialog", { name: tr("Export listing") });
    await user.click(within(dialog).getByRole("checkbox", { name: tr("Only entries matching the active filter \"{name}\"", { name: "JPEGs" }) }));
    await user.click(within(dialog).getByRole("switch", { name: new RegExp(esc(tr("Include everything below this folder"))) }));
    await user.click(within(dialog).getByRole("button", { name: tr("Export to file...") }));
    expect(await within(dialog).findByText(tr("Export finished"))).toBeInTheDocument();
    expect(dialog).toHaveTextContent(sentence("Exported {count} rows ({size}) to {path}.", { count: 13, size: W, path: W }));
  });

  it("shows why an export failed and lets the user try again", async () => {
    const platform = new TestPlatform();
    platform.saveAnswers.push("/no/such/folder/listing.csv", "/home/demo/Documents/listing.csv");
    const { user, mock } = await renderApp({ platform });
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Export listing") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Export listing") });
    await user.click(within(dialog).getByRole("button", { name: tr("Export to file...") }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("cannot create /no/such/folder/listing.csv");
    expect(mock.fs.isFile("/no/such/folder/listing.csv")).toBe(false);
    await user.click(within(dialog).getByRole("button", { name: tr("Export to file...") }));
    expect(await within(dialog).findByText(tr("Export finished"))).toBeInTheDocument();
    expect(mock.fs.isFile("/home/demo/Documents/listing.csv")).toBe(true);
  });
});
