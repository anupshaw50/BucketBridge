import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { renderApp } from "../../test/renderApp";

/**
 * Matches a field label exactly, tolerating the required "*" marker and the
 * "显示或隐藏…" toggle button: labels render the marker inline.
 */
function re(text: string): RegExp {
  return new RegExp(`^${text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(\\s*\\*)?$`);
}

async function openAddDialog(user: Awaited<ReturnType<typeof renderApp>>["user"]) {
  await user.click(await screen.findByRole("button", { name: tr("Accounts") }));
  const manager = await screen.findByRole("dialog", { name: tr("Accounts") });
  await user.click(within(manager).getByRole("button", { name: tr("Add account...") }));
  return screen.findByRole("dialog", { name: tr("Add account") });
}

async function choose(user: Awaited<ReturnType<typeof renderApp>>["user"], field: HTMLElement, option: string | RegExp) {
  await user.click(field);
  await user.click(await screen.findByRole("option", { name: option }));
}

async function chooseSource(
  user: Awaited<ReturnType<typeof renderApp>>["user"],
  dialog: HTMLElement,
  label: string | RegExp,
) {
  await user.type(within(dialog).getByRole("textbox", { name: tr("Label") }), "New account");
  await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
  await user.click(within(dialog).getByRole("radio", { name: label }));
}

describe("add an account that assumes a role", () => {
  it("checks the fields, adds the role through another account and edits its session", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    const dialog = await openAddDialog(user);
    await chooseSource(user, dialog, tr("Assume a role"));

    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(within(dialog).getByText(tr("Choose the account that signs the AssumeRole call."))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("Enter the role ARN, for example arn:aws:iam::123456789012:role/Name."))).toBeInTheDocument();
    // Neither keys nor a secret are asked for.
    expect(within(dialog).queryByLabelText(re(tr("Secret access key")))).not.toBeInTheDocument();

    await choose(user, within(dialog).getByRole("combobox", { name: tr("Sign the request with") }), "Demo");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Role ARN") }), "arn:aws:iam::123456789012:role/Writer");
    const minutes = within(dialog).getByRole("textbox", { name: tr("Session length (minutes)") });
    await user.clear(minutes);
    await user.type(minutes, "10");
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(within(dialog).getByText(tr("Enter a whole number of minutes from {min} to {max}.", { min: 15, max: 720 }))).toBeInTheDocument();
    await user.clear(minutes);
    await user.type(minutes, "90");
    await user.type(within(dialog).getByRole("textbox", { name: tr("External ID (optional)") }), "ext-1");
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));

    const review = within(dialog).getByRole("group", { name: tr("Review") });
    expect(within(review).getByText(tr("Assumed role"))).toBeInTheDocument();
    expect(within(review).getByText("arn:aws:iam::123456789012:role/Writer")).toBeInTheDocument();
    expect(within(review).getByText(tr("{name}, {n} minutes", { name: "bucketbridge", n: 90 }))).toBeInTheDocument();
    expect(within(review).queryByText(tr("Entered (hidden)"))).not.toBeInTheDocument();
    expect(within(review).queryByRole("button", { name: tr("Test connection") })).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Add account") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Add account") })).not.toBeInTheDocument());

    const added = (await mock.list_accounts()).accounts.find((account) => account.label === "New account");
    if (!added) throw new Error("account missing");
    expect(added.secret_ref).toBe("");
    expect(added.source).toMatchObject({
      type: "assume_role",
      role_arn: "arn:aws:iam::123456789012:role/Writer",
      duration_secs: 5400,
      external_id: "ext-1",
    });
    await expect(mock.list_buckets({ account_id: added.id })).resolves.toHaveLength(5);

    // Edit the session of the role that was just added.
    const manager = await screen.findByRole("dialog", { name: tr("Accounts") });
    expect(within(manager).getByText(tr("Role {name} through {via}", { name: "Writer", via: "Demo" }))).toBeInTheDocument();
    await user.click(within(manager).getByRole("button", { name: tr("Edit {label}", { label: "New account" }) }));
    const edit = await screen.findByRole("dialog", { name: tr("Edit account {label}", { label: "New account" }) });
    const length = within(edit).getByRole("textbox", { name: tr("Session length (minutes)") });
    expect(length).toHaveValue("90");
    await user.clear(length);
    await user.type(length, "120");
    await user.clear(within(edit).getByRole("textbox", { name: tr("External ID (optional)") }));
    await user.click(within(edit).getByRole("button", { name: tr("Save") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /Edit account/ })).not.toBeInTheDocument());
    const saved = (await mock.list_accounts()).accounts.find((account) => account.id === added.id);
    expect(saved?.source).toMatchObject({ type: "assume_role", duration_secs: 7200, external_id: null });
  });
});

describe("add an account with the instance role", () => {
  it("needs a successful check and then stores no secret", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    mock.credentials.instanceRoleAvailable = false;
    const dialog = await openAddDialog(user);
    await chooseSource(user, dialog, tr("Instance role"));
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(within(dialog).getByText(tr("Check that this machine has a role first."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Check this machine") }));
    expect(await within(dialog).findByText(/not an EC2 instance/)).toBeInTheDocument();

    mock.credentials.instanceRoleAvailable = true;
    await user.click(within(dialog).getByRole("button", { name: tr("Check this machine") }));
    expect(
      await within(dialog).findByText(
        tr("Found the role of this {machine} (access key {accessKeyId}).", {
          machine: tr("instance"),
          accessKeyId: "ASIAMOCKINSTANCE",
        }),
      ),
    ).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    expect(within(dialog).getByText(tr("Role of this EC2 instance or container"))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Add account") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Add account") })).not.toBeInTheDocument());
    const added = (await mock.list_accounts()).accounts.find((account) => account.label === "New account");
    expect(added?.source).toEqual({ type: "instance_role" });
    expect(added?.secret_ref).toBe("");
  });
});

describe("add an account through AWS SSO", () => {
  it("signs in with a code, picks the account and role and stores no token", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    const dialog = await openAddDialog(user);
    await chooseSource(user, dialog, "AWS SSO");

    // Signing in needs a portal first.
    await user.click(within(dialog).getByRole("button", { name: tr("Sign in with AWS SSO") }));
    expect(
      await within(dialog).findByText(tr("Enter the portal start URL (https://...) and the region first.")),
    ).toBeInTheDocument();

    await user.type(within(dialog).getByRole("textbox", { name: tr("Start URL") }), "https://example.awsapps.com/start");
    await user.type(within(dialog).getByRole("textbox", { name: tr("SSO region") }), "eu-west-1");
    await user.click(within(dialog).getByRole("button", { name: tr("Sign in with AWS SSO") }));
    expect(await within(dialog).findByText("WDJB-MJHT")).toBeInTheDocument();
    expect(within(dialog).getByText(tr("The page was opened in your browser."))).toBeInTheDocument();
    expect(mock.openedUrls).toContain("https://device.sso.example.com/?user_code=WDJB-MJHT");
    expect(await within(dialog).findByText(tr("Signed in. The token is kept in the AWS CLI cache (~/.aws/sso/cache), not in BucketBridge."), undefined, { timeout: 8000 })).toBeInTheDocument();

    await choose(user, within(dialog).getByRole("combobox", { name: tr("AWS account") }), /Production/);
    await choose(user, within(dialog).getByRole("combobox", { name: tr("Role") }), "Admin");
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    const review = within(dialog).getByRole("group", { name: tr("Review") });
    expect(within(review).getByText(tr("AWS SSO (IAM Identity Center)"))).toBeInTheDocument();
    expect(within(review).getByText("123456789012")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Add account") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Add account") })).not.toBeInTheDocument());
    const added = (await mock.list_accounts()).accounts.find((account) => account.label === "New account");
    expect(added?.source).toMatchObject({
      type: "sso",
      start_url: "https://example.awsapps.com/start",
      sso_region: "eu-west-1",
      account_id: "123456789012",
      role_name: "Admin",
    });
    expect(added?.secret_ref).toBe("");
  }, 20_000);

  it("shows the reason when the sign-in is refused and lets the user cancel", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    const dialog = await openAddDialog(user);
    await chooseSource(user, dialog, "AWS SSO");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Start URL") }), "http://insecure.example.com");
    await user.type(within(dialog).getByRole("textbox", { name: tr("SSO region") }), "eu-west-1");
    await user.click(within(dialog).getByRole("button", { name: tr("Sign in with AWS SSO") }));
    expect(
      await within(dialog).findByText(tr("Enter the portal start URL (https://...) and the region first.")),
    ).toBeInTheDocument();
    await user.clear(within(dialog).getByRole("textbox", { name: tr("Start URL") }));
    await user.type(within(dialog).getByRole("textbox", { name: tr("Start URL") }), "https://example.awsapps.com/start");
    await user.click(within(dialog).getByRole("button", { name: tr("Sign in with AWS SSO") }));
    expect(await within(dialog).findByText("WDJB-MJHT")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel sign-in") }));
    expect(within(dialog).queryByText("WDJB-MJHT")).not.toBeInTheDocument();
    // The account list stays locked until a sign-in completes.
    expect(within(dialog).getByRole("combobox", { name: tr("AWS account") })).toBeDisabled();
  });
});

describe("import from AWS config", () => {
  it("links the chosen profiles and leaves the unusable one", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Accounts") }));
    const manager = await screen.findByRole("dialog", { name: tr("Accounts") });
    await user.click(within(manager).getByRole("button", { name: tr("Import from AWS config...") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Import from AWS config") });
    expect(await within(dialog).findByText("prod-admin")).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: tr("Import {name}", { name: "broken" }) })).toBeDisabled();
    expect(within(dialog).getByText(/has no credentials, role, SSO/)).toBeInTheDocument();
    // Usable profiles start selected; leave one out.
    await user.click(within(dialog).getByRole("checkbox", { name: tr("Import {name}", { name: "prod-admin" }) }));
    await user.click(within(dialog).getByRole("button", { name: tr("Import") }));
    expect(
      await within(dialog).findByText(tr("Imported {n} profiles: {names}.", { n: 2, names: "dev, corp-sso" })),
    ).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Close") }));

    const accounts = (await mock.list_accounts()).accounts;
    const dev = accounts.find((account) => account.label === "dev");
    expect(dev?.source).toEqual({ type: "aws_profile", profile: "dev" });
    expect(dev?.secret_ref).toBe("");
    expect(dev?.region).toBe("eu-west-1");
    expect(accounts.some((account) => account.label === "prod-admin")).toBe(false);
    expect(within(manager).getByText(tr("AWS profile {profile}", { profile: "dev" }))).toBeInTheDocument();

    // A second visit shows the links.
    await user.click(within(manager).getByRole("button", { name: tr("Import from AWS config...") }));
    const again = await screen.findByRole("dialog", { name: tr("Import from AWS config") });
    expect((await within(again).findAllByText(tr("Already imported"))).length).toBe(2);
    expect(within(again).getByRole("checkbox", { name: tr("Import {name}", { name: "dev" }) })).toBeDisabled();
  });
});
