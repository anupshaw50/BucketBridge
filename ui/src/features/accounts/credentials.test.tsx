import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp } from "../../test/renderApp";

async function openAddDialog(user: Awaited<ReturnType<typeof renderApp>>["user"]) {
  await user.click(await screen.findByRole("button", { name: "Accounts" }));
  const manager = await screen.findByRole("dialog", { name: "Accounts" });
  await user.click(within(manager).getByRole("button", { name: "Add account..." }));
  return screen.findByRole("dialog", { name: "Add account" });
}

async function choose(user: Awaited<ReturnType<typeof renderApp>>["user"], field: HTMLElement, option: string | RegExp) {
  await user.click(field);
  await user.click(await screen.findByRole("option", { name: option }));
}

async function chooseSource(
  user: Awaited<ReturnType<typeof renderApp>>["user"],
  dialog: HTMLElement,
  label: string,
) {
  await user.type(within(dialog).getByRole("textbox", { name: /Label/ }), "New account");
  await user.click(within(dialog).getByRole("button", { name: "Next" }));
  await user.click(within(dialog).getByRole("radio", { name: label }));
}

describe("add an account that assumes a role", () => {
  it("checks the fields, adds the role through another account and edits its session", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    const dialog = await openAddDialog(user);
    await chooseSource(user, dialog, "Assume a role");

    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText("Choose the account that signs the AssumeRole call.")).toBeInTheDocument();
    expect(within(dialog).getByText(/Enter the role ARN/)).toBeInTheDocument();
    // Neither keys nor a secret are asked for.
    expect(within(dialog).queryByLabelText(/Secret access key/)).not.toBeInTheDocument();

    await choose(user, within(dialog).getByRole("combobox", { name: /Sign the request with/ }), "Demo");
    await user.type(within(dialog).getByRole("textbox", { name: /Role ARN/ }), "arn:aws:iam::123456789012:role/Writer");
    const minutes = within(dialog).getByRole("textbox", { name: /Session length/ });
    await user.clear(minutes);
    await user.type(minutes, "10");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText(/Enter a whole number of minutes from 15 to 720/)).toBeInTheDocument();
    await user.clear(minutes);
    await user.type(minutes, "90");
    await user.type(within(dialog).getByRole("textbox", { name: /External ID/ }), "ext-1");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));

    const review = within(dialog).getByRole("group", { name: "Review" });
    expect(within(review).getByText("Assumed role")).toBeInTheDocument();
    expect(within(review).getByText("arn:aws:iam::123456789012:role/Writer")).toBeInTheDocument();
    expect(within(review).getByText("bucketbridge, 90 minutes")).toBeInTheDocument();
    expect(within(review).queryByText("Entered (hidden)")).not.toBeInTheDocument();
    expect(within(review).queryByRole("button", { name: "Test connection" })).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).not.toBeInTheDocument());

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
    const manager = await screen.findByRole("dialog", { name: "Accounts" });
    expect(within(manager).getByText("Role Writer through Demo")).toBeInTheDocument();
    await user.click(within(manager).getByRole("button", { name: "Edit New account" }));
    const edit = await screen.findByRole("dialog", { name: "Edit account New account" });
    const length = within(edit).getByRole("textbox", { name: /Session length/ });
    expect(length).toHaveValue("90");
    await user.clear(length);
    await user.type(length, "120");
    await user.clear(within(edit).getByRole("textbox", { name: /External ID/ }));
    await user.click(within(edit).getByRole("button", { name: "Save" }));
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
    await chooseSource(user, dialog, "Instance role");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText("Check that this machine has a role first.")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Check this machine" }));
    expect(await within(dialog).findByText(/not an EC2 instance/)).toBeInTheDocument();

    mock.credentials.instanceRoleAvailable = true;
    await user.click(within(dialog).getByRole("button", { name: "Check this machine" }));
    expect(await within(dialog).findByText(/Found the role of this instance/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText("Role of this EC2 instance or container")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).not.toBeInTheDocument());
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
    await user.click(within(dialog).getByRole("button", { name: "Sign in with AWS SSO" }));
    expect(await within(dialog).findByText(/Enter the portal start URL/)).toBeInTheDocument();

    await user.type(within(dialog).getByRole("textbox", { name: /Start URL/ }), "https://example.awsapps.com/start");
    await user.type(within(dialog).getByRole("textbox", { name: /SSO region/ }), "eu-west-1");
    await user.click(within(dialog).getByRole("button", { name: "Sign in with AWS SSO" }));
    expect(await within(dialog).findByText("WDJB-MJHT")).toBeInTheDocument();
    expect(within(dialog).getByText(/The page was opened in your browser/)).toBeInTheDocument();
    expect(mock.openedUrls).toContain("https://device.sso.example.com/?user_code=WDJB-MJHT");
    expect(await within(dialog).findByText(/Signed in\./, undefined, { timeout: 8000 })).toBeInTheDocument();

    await choose(user, within(dialog).getByRole("combobox", { name: /AWS account/ }), /Production/);
    await choose(user, within(dialog).getByRole("combobox", { name: /^Role$/ }), "Admin");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    const review = within(dialog).getByRole("group", { name: "Review" });
    expect(within(review).getByText("AWS SSO (IAM Identity Center)")).toBeInTheDocument();
    expect(within(review).getByText("123456789012")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).not.toBeInTheDocument());
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
    await user.type(within(dialog).getByRole("textbox", { name: /Start URL/ }), "http://insecure.example.com");
    await user.type(within(dialog).getByRole("textbox", { name: /SSO region/ }), "eu-west-1");
    await user.click(within(dialog).getByRole("button", { name: "Sign in with AWS SSO" }));
    expect(await within(dialog).findByText(/Enter the portal start URL/)).toBeInTheDocument();
    await user.clear(within(dialog).getByRole("textbox", { name: /Start URL/ }));
    await user.type(within(dialog).getByRole("textbox", { name: /Start URL/ }), "https://example.awsapps.com/start");
    await user.click(within(dialog).getByRole("button", { name: "Sign in with AWS SSO" }));
    expect(await within(dialog).findByText("WDJB-MJHT")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel sign-in" }));
    expect(within(dialog).queryByText("WDJB-MJHT")).not.toBeInTheDocument();
    // The account list stays locked until a sign-in completes.
    expect(within(dialog).getByRole("combobox", { name: /AWS account/ })).toBeDisabled();
  });
});

describe("import from AWS config", () => {
  it("links the chosen profiles and leaves the unusable one", async () => {
    const { user, mock } = await renderApp();
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: "Accounts" }));
    const manager = await screen.findByRole("dialog", { name: "Accounts" });
    await user.click(within(manager).getByRole("button", { name: "Import from AWS config..." }));
    const dialog = await screen.findByRole("dialog", { name: "Import from AWS config" });
    expect(await within(dialog).findByText("prod-admin")).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "Import broken" })).toBeDisabled();
    expect(within(dialog).getByText(/has no credentials, role, SSO/)).toBeInTheDocument();
    // Usable profiles start selected; leave one out.
    await user.click(within(dialog).getByRole("checkbox", { name: "Import prod-admin" }));
    await user.click(within(dialog).getByRole("button", { name: "Import" }));
    expect(await within(dialog).findByText(/Imported 2 profiles: dev, corp-sso\./)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Close" }));

    const accounts = (await mock.list_accounts()).accounts;
    const dev = accounts.find((account) => account.label === "dev");
    expect(dev?.source).toEqual({ type: "aws_profile", profile: "dev" });
    expect(dev?.secret_ref).toBe("");
    expect(dev?.region).toBe("eu-west-1");
    expect(accounts.some((account) => account.label === "prod-admin")).toBe(false);
    expect(within(manager).getByText("AWS profile dev")).toBeInTheDocument();

    // A second visit shows the links.
    await user.click(within(manager).getByRole("button", { name: "Import from AWS config..." }));
    const again = await screen.findByRole("dialog", { name: "Import from AWS config" });
    expect((await within(again).findAllByText("Already imported")).length).toBe(2);
    expect(within(again).getByRole("checkbox", { name: "Import dev" })).toBeDisabled();
  });
});
