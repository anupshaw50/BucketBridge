import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { CorsConfig } from "../../api/types";
import { tr } from "../../i18n";
import type { MockBackend } from "../../mock/mockBackend";
import { renderApp } from "../../test/renderApp";
import { renameBucketInPolicy } from "../../mock/bucketCloneMock";

const POLICY = JSON.stringify({
  Version: "2012-10-17",
  Statement: [{ Effect: "Allow", Principal: "*", Action: "s3:GetObject", Resource: "arn:aws:s3:::demo-photos/*" }],
});
const CORS: CorsConfig = {
  rules: [{ id: null, allowed_methods: ["GET"], allowed_origins: ["*"], allowed_headers: [], expose_headers: [], max_age_seconds: null }],
};

/** Regex that matches the given text with anything in the place of a {placeholder}. */
function pattern(template: string): RegExp {
  const text = tr(template).replace(/\{(\w+)\}/g, "§§§");
  return new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/§§§/g, ".*"));
}

async function accountId(mock: MockBackend): Promise<string> {
  const [account] = (await mock.list_accounts()).accounts;
  if (account === undefined) {
    throw new Error("no account");
  }
  return account.id;
}

async function configure(mock: MockBackend): Promise<void> {
  const id = await accountId(mock);
  await mock.put_bucket_policy({ account_id: id, bucket: "demo-photos", policy: POLICY });
  await mock.put_bucket_cors({ account_id: id, bucket: "demo-photos", config: CORS });
}

async function openClone(user: UserEvent): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
  const row = (await within(tree).findByText("demo-photos")).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error("no tree row");
  }
  fireEvent.contextMenu(row);
  await user.click(await screen.findByRole("menuitem", { name: tr("Clone bucket…") }));
  return screen.findByRole("dialog", { name: tr("Clone bucket: {bucket}", { bucket: "demo-photos" }) });
}

describe("policy renaming", () => {
  it("renames the bucket in resource ARNs only", () => {
    const renamed = JSON.parse(
      renameBucketInPolicy(
        JSON.stringify({ Statement: [{ Resource: ["arn:aws:s3:::src/*", "arn:aws:s3:::src", "arn:aws:s3:::src-logs/*"] }] }),
        "src",
        "dst",
      ),
    ) as { Statement: { Resource: string[] }[] };
    expect(renamed.Statement[0]?.Resource).toEqual(["arn:aws:s3:::dst/*", "arn:aws:s3:::dst", "arn:aws:s3:::src-logs/*"]);
  });
});

describe("clone bucket dialog", () => {
  it("lists what the source has and clones the chosen settings with the objects", async () => {
    const { user, mock } = await renderApp({ prepare: configure });
    const dialog = await openClone(user);
    const table = await within(dialog).findByRole("table", { name: tr("Settings to copy") });
    expect(within(table).getByText(tr("Statements: {count}", { count: 1 }))).toBeInTheDocument();
    expect(within(table).getByText(tr("Rules: {count}", { count: 1 }))).toBeInTheDocument();
    const policy = within(table).getByRole("checkbox", { name: tr("Bucket policy") });
    expect(policy).toBeChecked();
    // A setting the source does not have cannot be chosen.
    expect(within(table).getByRole("checkbox", { name: tr("Bucket tags") })).toBeDisabled();
    expect(within(dialog).getAllByText(tr("Not set on the source")).length).toBeGreaterThan(0);

    // The count of the objects that will be copied.
    await user.click(within(dialog).getByRole("button", { name: tr("Count objects") }));
    expect(await within(dialog).findByText(pattern("{count} objects, {size}"))).toBeInTheDocument();

    await user.clear(within(dialog).getByRole("textbox", { name: new RegExp(tr("New bucket name")) }));
    await user.type(within(dialog).getByRole("textbox", { name: new RegExp(tr("New bucket name")) }), "demo-photos-clone");
    await user.click(within(table).getByRole("checkbox", { name: tr("CORS rules") }));
    const before = (await mock.list_tasks()).length;
    await user.click(within(dialog).getByRole("button", { name: tr("Clone bucket") }));

    expect(await within(dialog).findByText(tr("Bucket {name} was created.", { name: "demo-photos-clone" }))).toBeInTheDocument();
    const results = within(dialog).getByRole("table", { name: tr("Settings copied") });
    expect(within(results).getByText(tr("Bucket policy"))).toBeInTheDocument();
    expect(within(results).queryByText(tr("CORS rules"))).toBeNull();

    const id = await accountId(mock);
    const policyText = await mock.get_bucket_policy({ account_id: id, bucket: "demo-photos-clone" });
    expect(policyText).toContain("arn:aws:s3:::demo-photos-clone/*");
    expect(policyText).not.toContain("arn:aws:s3:::demo-photos/");
    expect(await mock.get_bucket_cors({ account_id: id, bucket: "demo-photos-clone" })).toBeNull();
    expect((await mock.list_tasks()).length).toBeGreaterThan(before);
    expect((await mock.list_buckets({ account_id: id })).some((bucket) => bucket.name === "demo-photos-clone")).toBe(true);
    // The copy runs in the queue and its progress is shown.
    expect(within(dialog).getByRole("progressbar", { name: tr("Copy progress") })).toBeInTheDocument();
  });

  it("checks the name, and an existing bucket is a conflict", async () => {
    const { user } = await renderApp({ prepare: configure });
    const dialog = await openClone(user);
    await within(dialog).findByRole("table", { name: tr("Settings to copy") });
    const name = within(dialog).getByRole("textbox", { name: new RegExp(tr("New bucket name")) });
    await user.clear(name);
    await user.type(name, "AB");
    expect(within(dialog).getByRole("button", { name: tr("Clone bucket") })).toBeDisabled();
    await user.clear(name);
    await user.type(name, "demo-backups");
    await user.click(within(dialog).getByRole("button", { name: tr("Clone bucket") }));
    await waitFor(() => expect(within(dialog).getByRole("alert")).toBeInTheDocument());
    expect(within(dialog).queryByText(tr("Bucket {name} was created.", { name: "demo-backups" }))).toBeNull();
  });

  it("copies only a prefix and needs a folder for it", async () => {
    const { user, mock } = await renderApp({ prepare: configure });
    const dialog = await openClone(user);
    await within(dialog).findByRole("table", { name: tr("Settings to copy") });
    const prefix = within(dialog).getByRole("textbox", { name: tr("Only objects under this prefix (optional)") });
    await user.type(prefix, "photos");
    expect(within(dialog).getByText(tr("A prefix is a folder: it ends with /."))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Clone bucket") })).toBeDisabled();
    await user.type(prefix, "/");
    const id = await accountId(mock);
    const before = (await mock.list_tasks()).length;
    await user.click(within(dialog).getByRole("button", { name: tr("Clone bucket") }));
    await within(dialog).findByText(tr("Bucket {name} was created.", { name: "demo-photos-copy" }));
    const tasks = await mock.list_tasks();
    const added = tasks.slice(before);
    expect(added.every((task) => task.dst?.type === "s3" && task.dst.bucket === "demo-photos-copy" && task.dst.key.startsWith("photos/"))).toBe(true);
    expect((await mock.list_buckets({ account_id: id })).some((bucket) => bucket.name === "demo-photos-copy")).toBe(true);
  });
});
