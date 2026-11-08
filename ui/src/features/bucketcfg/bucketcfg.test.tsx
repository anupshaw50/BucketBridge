import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import type { MockBackend } from "../../mock/mockBackend";
import { renderApp, statusBar } from "../../test/renderApp";

/** The tree pane of a rendered app (its name follows the interface language). */
async function findTree(): Promise<HTMLElement> {
  return screen.findByRole("tree", { name: tr("Buckets and folders") });
}

/** Opens a bucket from the tree and waits for its listing (local variant of test/renderApp's helper). */
async function openBucket(user: UserEvent, name: string): Promise<void> {
  const tree = await findTree();
  const item = await within(tree).findByText(name);
  await user.click(item);
  await screen.findByRole("grid", { name: tr("Objects") });
}

/** Matches the start of the accessible name of a field; the description may follow it. */
function nameStartsWith(text: string): RegExp {
  return new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
}

async function demoAccount(mock: MockBackend): Promise<string> {
  const id = (await mock.list_accounts()).default_account_id;
  if (id === null) {
    throw new Error("no default account");
  }
  return id;
}

/** Opens a bucket and its properties from the bucket menu of the toolbar. */
async function openProperties(user: UserEvent, bucket: string): Promise<HTMLElement> {
  await openBucket(user, bucket);
  await user.click(screen.getByRole("button", { name: tr("Bucket actions") }));
  await user.click(await screen.findByRole("menuitem", { name: tr("Properties of {name}...", { name: bucket }) }));
  return screen.findByRole("dialog", { name: tr("Bucket properties: {bucket}", { bucket }) });
}

async function selectTab(user: UserEvent, dialog: HTMLElement, name: string): Promise<void> {
  await user.click(within(dialog).getByRole("tab", { name }));
}

function policyEditor(dialog: HTMLElement): HTMLTextAreaElement {
  return within(dialog).getByRole<HTMLTextAreaElement>("textbox", { name: tr("Bucket policy (JSON)") });
}

describe("bucket properties: policy", () => {
  it("shows the policy pretty-printed, validates, formats, saves and re-reads it", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const dialog = await openProperties(user, "demo-website");
    const editor = await within(dialog).findByRole<HTMLTextAreaElement>("textbox", { name: tr("Bucket policy (JSON)") });
    expect(editor.value).toContain('"Sid": "PublicRead"');
    expect(editor.value.startsWith('{\n  "Version": "2012-10-17"')).toBe(true);

    // A policy that is wrong lists every problem with its path and is not saved.
    fireEvent.change(editor, {
      target: { value: '{"Version": "2012-10-17", "Statement": [{"Effect": "Maybe", "Principal": "*", "Action": "s3:*"}]}' },
    });
    await user.click(within(dialog).getByRole("button", { name: tr("Validate") }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent(tr("{n} problems", { n: 2 }));
    expect(alert).toHaveTextContent('Statement[0].Effect: must be "Allow" or "Deny" (found "Maybe")');
    expect(alert).toHaveTextContent('Statement[0]: needs "Resource" or "NotResource"');
    await user.click(within(dialog).getByRole("button", { name: tr("Save policy") }));
    await within(dialog).findByRole("alert");
    expect(await mock.get_bucket_policy({ account_id: id, bucket: "demo-website" })).toContain('"Sid": "PublicRead"');

    // Fixed, formatted, validated and saved; the editor then shows what S3 stores.
    fireEvent.change(editor, {
      target: {
        value:
          '{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Principal":"*","Action":"s3:DeleteObject","Resource":"arn:aws:s3:::demo-website/*"}]}',
      },
    });
    await user.click(within(dialog).getByRole("button", { name: tr("Format") }));
    await waitFor(() => expect(policyEditor(dialog).value).toContain('\n  "Statement": [\n    {\n      "Effect": "Deny",'));
    await user.click(within(dialog).getByRole("button", { name: tr("Validate") }));
    expect(await within(dialog).findByText(tr("The policy is valid."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Save policy") }));
    await waitFor(() => expect(statusBar().getByText(tr("The policy of {bucket} was saved.", { bucket: "demo-website" }))).toBeInTheDocument());
    const stored = await mock.get_bucket_policy({ account_id: id, bucket: "demo-website" });
    const saved = JSON.parse(stored ?? "{}") as { Statement: { Action: string }[] };
    expect(saved.Statement[0]?.Action).toBe("s3:DeleteObject");
    await waitFor(() => expect(policyEditor(dialog).value).toBe(stored));
    expect(within(dialog).getByRole("button", { name: tr("Save policy") })).toBeDisabled();
  });

  it("formats text that is not valid JSON as a problem, and deletes the policy after a confirmation", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const dialog = await openProperties(user, "demo-website");
    const editor = await within(dialog).findByRole<HTMLTextAreaElement>("textbox", { name: tr("Bucket policy (JSON)") });
    fireEvent.change(editor, { target: { value: '{"Version": ' } });
    await user.click(within(dialog).getByRole("button", { name: tr("Format") }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("not valid JSON: unexpected end of input at line 1 column 13");

    await user.click(within(dialog).getByRole("button", { name: tr("Delete policy") }));
    const confirm = await screen.findByRole("dialog", { name: tr("Delete bucket policy") });
    // Cancelling keeps the policy.
    await user.click(within(confirm).getByRole("button", { name: tr("Cancel") }));
    expect(await mock.get_bucket_policy({ account_id: id, bucket: "demo-website" })).not.toBeNull();
    await user.click(within(dialog).getByRole("button", { name: tr("Delete policy") }));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Delete bucket policy") })).getByRole("button", { name: tr("Delete policy") }));
    await waitFor(async () => expect(await mock.get_bucket_policy({ account_id: id, bucket: "demo-website" })).toBeNull());
    expect(await within(dialog).findByText(tr("This bucket has no policy. Write one below and save it."))).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: tr("Delete policy") })).not.toBeInTheDocument();
  });
});

describe("bucket properties: the other settings", () => {
  it("edits the four toggles of the public access block and removes the configuration", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const ref = { account_id: id, bucket: "demo-photos" };
    const dialog = await openProperties(user, "demo-photos");
    await selectTab(user, dialog, tr("Public access"));
    const acls = await within(dialog).findByRole("switch", { name: nameStartsWith(tr("Block new public ACLs")) });
    expect(acls).toBeChecked();
    expect(within(dialog).getByRole("switch", { name: nameStartsWith(tr("Restrict access to buckets with a public policy")) })).toBeChecked();
    expect(within(dialog).getByRole("button", { name: tr("Save") })).toBeDisabled();

    await user.click(acls);
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect((await mock.get_public_access_block(ref))?.block_public_acls).toBe(false));
    expect(await mock.get_public_access_block(ref)).toEqual({
      block_public_acls: false,
      ignore_public_acls: true,
      block_public_policy: true,
      restrict_public_buckets: true,
    });
    await waitFor(() => expect(within(dialog).getByRole("switch", { name: nameStartsWith(tr("Block new public ACLs")) })).not.toBeChecked());

    await user.click(within(dialog).getByRole("button", { name: tr("Remove configuration") }));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Remove public access block") })).getByRole("button", { name: tr("Remove configuration") }));
    await waitFor(async () => expect(await mock.get_public_access_block(ref)).toBeNull());
    expect(await within(dialog).findByText(tr("No public access block is configured, so S3 blocks nothing on this bucket."))).toBeInTheDocument();
  });

  it("enables, then suspends versioning, and shows MFA delete read-only", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const ref = { account_id: id, bucket: "demo-empty" };
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("Versioning"));
    expect(await within(dialog).findByText(tr("Versioning has never been enabled on this bucket."))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Save") })).toBeDisabled();
    await user.click(within(dialog).getByRole("radio", { name: tr("Enabled") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect((await mock.get_bucket_versioning(ref)).status).toBe("enabled"));
    expect(await within(dialog).findByText(tr("Versioning is enabled: every change to an object keeps the previous version."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("radio", { name: tr("Suspended") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect((await mock.get_bucket_versioning(ref)).status).toBe("suspended"));
    // MFA delete is reported, not editable.
    expect(within(dialog).getByText(tr("Disabled"))).toBeInTheDocument();
    expect(within(dialog).queryByRole("switch", { name: /MFA/ })).not.toBeInTheDocument();
  });

  it("turns server access logging on, changes its target and turns it off", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const ref = { account_id: id, bucket: "demo-empty" };
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("Logging"));
    await user.click(await within(dialog).findByRole("switch", { name: tr("Log requests made to this bucket") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    expect(await within(dialog).findByText(tr("Enter the bucket that receives the logs."))).toBeInTheDocument();
    await user.type(within(dialog).getByRole("textbox", { name: tr("Target bucket") }), "demo-logs");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Log prefix") }), "empty/");
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect(await mock.get_bucket_logging(ref)).toEqual({ target_bucket: "demo-logs", target_prefix: "empty/" }));
    await waitFor(() => expect(within(dialog).getByRole("textbox", { name: tr("Log prefix") })).toHaveValue("empty/"));

    await user.click(within(dialog).getByRole("switch", { name: tr("Log requests made to this bucket") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect(await mock.get_bucket_logging(ref)).toBeNull());
    expect(await within(dialog).findByRole("switch", { name: tr("Log requests made to this bucket") })).not.toBeChecked();
  });

  it("hosts a website with a routing rule, rejects a wrong one and disables hosting after a confirmation", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const ref = { account_id: id, bucket: "demo-empty" };
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("Website"));
    expect(await within(dialog).findByText(tr("This bucket does not serve a website."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("radio", { name: tr("Host a static website") }));
    expect(within(dialog).getByRole("textbox", { name: tr("Index document") })).toHaveValue("index.html");
    await user.click(within(dialog).getByRole("button", { name: tr("Add routing rule") }));
    const rule = within(within(dialog).getByRole("group", { name: tr("Routing rule {number}", { number: 1 }) }));
    await user.type(rule.getByRole("textbox", { name: tr("If the key starts with") }), "docs/");

    // A rule without a redirect is a problem; nothing is sent.
    await user.click(within(dialog).getByRole("button", { name: tr("Save website settings") }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("routing rule 1: the redirect needs a protocol, host, replacement key or prefix, or redirect code");
    expect(await mock.get_bucket_website(ref)).toBeNull();

    await user.type(rule.getByRole("textbox", { name: tr("Replace the key prefix with") }), "documents/");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Error document") }), "error.html");
    await user.click(within(dialog).getByRole("button", { name: tr("Save website settings") }));
    await waitFor(async () => expect(await mock.get_bucket_website(ref)).not.toBeNull());
    expect(await mock.get_bucket_website(ref)).toEqual({
      type: "hosting",
      index_document: "index.html",
      error_document: "error.html",
      routing_rules: [
        {
          condition: { key_prefix_equals: "docs/", http_error_code_returned_equals: null },
          redirect: {
            protocol: null,
            host_name: null,
            replace_key_prefix_with: "documents/",
            replace_key_with: null,
            http_redirect_code: null,
          },
        },
      ],
    });
    expect(await within(dialog).findByText(tr("This bucket serves a static website."))).toBeInTheDocument();
    expect(within(dialog).getByRole("textbox", { name: tr("If the key starts with") })).toHaveValue("docs/");

    await user.click(within(dialog).getByRole("radio", { name: tr("Off") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Disable website hosting") }));
    const confirm = await screen.findByRole("dialog", { name: tr("Disable website hosting") });
    await user.click(within(confirm).getByRole("button", { name: tr("Cancel") }));
    expect(await mock.get_bucket_website(ref)).not.toBeNull();
    await user.click(within(dialog).getByRole("button", { name: tr("Disable website hosting") }));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Disable website hosting") })).getByRole("button", { name: tr("Disable hosting") }));
    await waitFor(async () => expect(await mock.get_bucket_website(ref)).toBeNull());
  });

  it("redirects all requests to another host", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("Website"));
    await user.click(await within(dialog).findByRole("radio", { name: tr("Redirect all requests") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Host name") }), "www.example.com");
    await user.click(within(dialog).getByRole("button", { name: tr("Save website settings") }));
    await waitFor(async () =>
      expect(await mock.get_bucket_website({ account_id: id, bucket: "demo-empty" })).toEqual({
        type: "redirect",
        host_name: "www.example.com",
        protocol: null,
      }),
    );
  });

  it("enables and suspends transfer acceleration", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    const ref = { account_id: id, bucket: "demo-empty" };
    const dialog = await openProperties(user, "demo-empty");
    await selectTab(user, dialog, tr("Acceleration"));
    expect(await within(dialog).findByText(tr("Transfer acceleration has never been configured on this bucket."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("radio", { name: tr("Enabled") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect(await mock.get_bucket_acceleration(ref)).toBe("enabled"));
    expect(await within(dialog).findByText(tr("Transfer acceleration is enabled."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("radio", { name: tr("Suspended") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Save") }));
    await waitFor(async () => expect(await mock.get_bucket_acceleration(ref)).toBe("suspended"));
  });
});

describe("bucket properties: settings fail on their own", () => {
  it("shows a refused setting as an error with a retry, and keeps the other tabs working", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    mock.failBucketSection(id, "demo-photos", "policy", { code: "auth", message: "Access Denied (AccessDenied)" });
    const dialog = await openProperties(user, "demo-photos");
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent(tr("Access denied"));
    expect(alert).toHaveTextContent("reading the policy of bucket 'demo-photos': Access Denied (AccessDenied)");
    expect(within(dialog).queryByRole("textbox", { name: tr("Bucket policy (JSON)") })).not.toBeInTheDocument();
    // Trying again asks the backend again; it still refuses.
    await user.click(within(alert).getByRole("button", { name: tr("Try again") }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("Access Denied (AccessDenied)");

    await selectTab(user, dialog, tr("Public access"));
    expect(await within(dialog).findByRole("switch", { name: nameStartsWith(tr("Block new public ACLs")) })).toBeChecked();
    await selectTab(user, dialog, tr("Versioning"));
    expect(await within(dialog).findByText(tr("Versioning has never been enabled on this bucket."))).toBeInTheDocument();
  });

  it("tells a server without a feature from a failure, and has no acceleration on a custom endpoint", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Account: {label}", { label: "Demo" }) }));
    await user.click(await screen.findByRole("menuitem", { name: /Archive \(custom endpoint\)/ }));
    const dialog = await openProperties(user, "archive-2025");
    // Policy and versioning are implemented.
    expect(await within(dialog).findByRole("textbox", { name: tr("Bucket policy (JSON)") })).toBeInTheDocument();
    await selectTab(user, dialog, tr("Versioning"));
    expect(await within(dialog).findByText(tr("Versioning is enabled: every change to an object keeps the previous version."))).toBeInTheDocument();
    // The rest is reported as not supported, not as an error.
    for (const [tab, feature] of [
      [tr("Public access"), "the public access block"],
      [tr("Logging"), "server access logging"],
      [tr("Website"), "static website hosting"],
    ] as const) {
      await selectTab(user, dialog, tab);
      expect(await within(dialog).findByText(new RegExp(`This server does not support ${feature}`))).toBeInTheDocument();
    }
    await selectTab(user, dialog, tr("Acceleration"));
    expect(
      await within(dialog).findByText("Transfer acceleration is available on AWS S3 only; this account uses a custom endpoint."),
    ).toBeInTheDocument();
    expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
  });
});

describe("external buckets", () => {
  async function treeItem(name: string): Promise<HTMLElement> {
    const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
    const item = (await within(tree).findByText(name)).closest<HTMLElement>('[role="treeitem"]');
    if (item === null) {
      throw new Error(`no tree item for ${name}`);
    }
    return item;
  }

  it("adds a bucket the account cannot list, marks it External, opens it and removes it again", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    await screen.findByText("demo-photos");
    expect(screen.queryByText("partner-exchange")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: tr("Add external bucket") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Add external bucket") });
    // A bucket that does not exist is refused, with the reason.
    await user.type(within(dialog).getByRole("textbox", { name: tr("Bucket name") }), "no-such-bucket");
    await user.click(within(dialog).getByRole("button", { name: tr("Add bucket") }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("bucket 'no-such-bucket' does not exist");
    await user.clear(within(dialog).getByRole("textbox", { name: tr("Bucket name") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Bucket name") }), "bad name!");
    expect(within(dialog).getByText(tr("Use letters, digits, dots, hyphens and underscores, starting with a letter or digit."))).toBeInTheDocument();
    await user.clear(within(dialog).getByRole("textbox", { name: tr("Bucket name") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Bucket name") }), "partner-exchange");
    await user.click(within(dialog).getByRole("button", { name: tr("Add bucket") }));

    await waitFor(() => expect(statusBar().getByText(tr("External bucket {name} added.", { name: "partner-exchange" }))).toBeInTheDocument());
    const item = await treeItem("partner-exchange");
    expect(within(item).getByText(tr("External"))).toBeInTheDocument();
    await screen.findByRole("grid", { name: tr("Objects") });
    expect(await screen.findByText("inbox")).toBeInTheDocument();
    expect(await mock.list_external_buckets({ account_id: id })).toEqual(["partner-exchange"]);

    await user.click(within(item).getByRole("button", { name: tr("Remove external bucket {name}", { name: "partner-exchange" }) }));
    const confirm = await screen.findByRole("dialog", { name: tr("Remove external bucket") });
    await user.click(within(confirm).getByRole("button", { name: tr("Remove") }));
    await waitFor(() => expect(screen.queryByText("partner-exchange")).not.toBeInTheDocument());
    expect(await mock.list_external_buckets({ account_id: id })).toEqual([]);
  });

  it("offers to add an external bucket when the account may not list buckets", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    await screen.findByText("demo-photos");
    mock.denyListing(id);
    await user.click(screen.getByRole("button", { name: tr("Reload buckets") }));
    const alert = await screen.findByRole("alert", { name: tr("Buckets unavailable") });
    expect(alert).toHaveTextContent("Access Denied");
    await user.click(within(alert).getByRole("button", { name: tr("Add external bucket") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Add external bucket") });
    await user.type(within(dialog).getByRole("textbox", { name: tr("Bucket name") }), "partner-exchange");
    await user.click(within(dialog).getByRole("button", { name: tr("Add bucket") }));
    // Listing is still refused, so the pinned bucket is all there is.
    const item = await treeItem("partner-exchange");
    expect(within(item).getByText(tr("External"))).toBeInTheDocument();
    expect(screen.queryByText("demo-photos")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert", { name: tr("Buckets unavailable") })).not.toBeInTheDocument();
  });

  it("removes the pin of the open bucket from the bucket menu", async () => {
    const { user, mock } = await renderApp();
    const id = await demoAccount(mock);
    await mock.add_external_bucket({ account_id: id, name: "partner-exchange" });
    await user.click(screen.getByRole("button", { name: tr("Reload buckets") }));
    await openBucket(user, "partner-exchange");
    await user.click(screen.getByRole("button", { name: tr("Bucket actions") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Remove external bucket {name}...", { name: "partner-exchange" }) }));
    await user.click(within(await screen.findByRole("dialog", { name: tr("Remove external bucket") })).getByRole("button", { name: tr("Remove") }));
    await waitFor(async () => expect(await mock.list_external_buckets({ account_id: id })).toEqual([]));
    await waitFor(() => expect(screen.queryByText("partner-exchange")).not.toBeInTheDocument());
  });
});
