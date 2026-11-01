import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { MockBackend } from "../../mock/mockBackend";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";
import { AVAILABLE } from "../menus/available";
import { keyProblem } from "./expiry";

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: "Buckets and folders" });
  const row = (await within(tree).findByText(name)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${name}`);
  }
  return row;
}

async function openWizard(user: UserEvent): Promise<HTMLElement> {
  fireEvent.contextMenu(await bucketRow("demo-photos"));
  await user.click(screen.getByRole("menuitem", { name: "Sharing wizard…" }));
  return screen.findByRole("dialog", { name: "Sharing wizard: demo-photos" });
}

/** Chooses Bitly with a token on the Sharing page of the settings. */
async function chooseBitly(user: UserEvent): Promise<void> {
  await user.click(await screen.findByRole("button", { name: "Settings" }));
  const dialog = await screen.findByRole("dialog", { name: "Settings" });
  await user.click(within(dialog).getByRole("button", { name: "Sharing" }));
  expect(within(dialog).getByText(/is sent to it/)).toBeInTheDocument();
  await user.click(within(dialog).getByRole("combobox", { name: "URL shortener" }));
  await user.click(await screen.findByRole("option", { name: "Bitly" }));
  await user.click(within(dialog).getByRole("button", { name: "Set token..." }));
  await user.type(within(dialog).getByLabelText("New token"), "secret-token");
  await user.click(within(dialog).getByRole("button", { name: "Save token" }));
  await within(dialog).findByText(/A token is stored/);
  await user.click(within(dialog).getByRole("button", { name: "OK" }));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Settings" })).toBeNull());
}

describe("sharing wizard", () => {
  it("is available and validates the key", () => {
    expect(AVAILABLE["bucket.sharing-wizard"]).toBe(true);
    expect(keyProblem("")).not.toBeNull();
    expect(keyProblem("docs/")).toMatch(/not a folder/);
    expect(keyProblem("docs/a.txt")).toBeNull();
  });

  it("walks object, access and link, and shortening is off by default", async () => {
    const { user, mock } = await renderApp();
    const dialog = await openWizard(user);
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText("Enter the key of the object to share.")).toBeInTheDocument();
    await user.type(within(dialog).getByRole("combobox", { name: "Object key" }), "readme.md");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText(/s3:\/\/demo-photos\/readme.md/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("radio", { name: "Upload (PUT)" }));
    const amount = within(dialog).getByRole("textbox", { name: "Valid for" });
    await user.clear(amount);
    await user.type(amount, "30");
    await user.click(within(dialog).getByRole("combobox", { name: "Unit" }));
    await user.click(await screen.findByRole("option", { name: "Minutes" }));
    await user.click(within(dialog).getByRole("button", { name: "Create link" }));
    const link = await within(dialog).findByRole("textbox", { name: "Link" });
    expect((link as HTMLTextAreaElement).value).toContain("X-Amz-Expires=1800");
    expect(within(dialog).getByText(/Upload with an HTTP PUT request/)).toBeInTheDocument();
    expect(within(dialog).getByText(/URL shortening is off/)).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: "Shorten" })).toBeNull();
    expect(mock.shortenedLinks()).toEqual([]);
    // Back keeps the answers.
    await user.click(within(dialog).getByRole("button", { name: "Back" }));
    expect(within(dialog).getByRole("radio", { name: "Upload (PUT)" })).toBeChecked();
    await user.click(within(dialog).getByRole("button", { name: "Back" }));
    expect(within(dialog).getByRole("combobox", { name: "Object key" })).toHaveValue("readme.md");
  });

  it("refuses a folder key", async () => {
    const { user } = await renderApp();
    const dialog = await openWizard(user);
    await user.type(within(dialog).getByRole("combobox", { name: "Object key" }), "docs/");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    expect(within(dialog).getByText("Choose an object, not a folder.")).toBeInTheDocument();
  });

  it("shortens the link only when asked, naming the service and what leaves the machine", async () => {
    const { user, mock, platform } = await renderApp();
    await chooseBitly(user);
    const dialog = await openWizard(user);
    await user.type(within(dialog).getByRole("combobox", { name: "Object key" }), "readme.md");
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    await user.click(within(dialog).getByRole("button", { name: "Create link" }));
    await within(dialog).findByRole("textbox", { name: "Link" });
    expect(within(dialog).getByText(/sends the whole link, including its signature, to Bitly/)).toBeInTheDocument();
    expect(mock.shortenedLinks()).toEqual([]);
    await user.click(within(dialog).getByRole("button", { name: "Shorten" }));
    const short = await within(dialog).findByRole("textbox", { name: "Short link" });
    expect((short as HTMLInputElement).value).toBe("https://bit.ly/m1");
    expect(mock.shortenedLinks()).toHaveLength(1);
    expect(mock.shortenedLinks()[0]).toContain("X-Amz-Signature=");
    await user.click(within(dialog).getByRole("button", { name: "Copy short link" }));
    expect(platform.copied.at(-1)).toBe("https://bit.ly/m1");
  });

  it("adds Shorten to the share link dialog", async () => {
    const { user, mock } = await renderApp();
    await chooseBitly(user);
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.click(screen.getByRole("button", { name: "Share link" }));
    const dialog = await screen.findByRole("dialog", { name: "Share link" });
    await user.click(within(dialog).getByRole("button", { name: "Create link" }));
    await within(dialog).findByRole("textbox", { name: "Link" });
    await user.click(within(dialog).getByRole("button", { name: "Shorten" }));
    expect(await within(dialog).findByRole("textbox", { name: "Short link" })).toBeInTheDocument();
    expect(mock.shortenedLinks()).toHaveLength(1);
  });
});

describe("shortener mock", () => {
  const url = "https://b.s3.example.com/k?X-Amz-Signature=abc";

  it("sends nothing without a service, a token for Bitly or an http link", async () => {
    const mock = new MockBackend({ autoRun: false, latencyMs: 0 });
    await expect(mock.shorten_url({ url })).rejects.toMatchObject({ code: "usage" });
    const settings = await mock.get_settings();
    settings.shortener.provider = "bitly";
    await mock.save_settings({ settings });
    await expect(mock.shorten_url({ url })).rejects.toMatchObject({ code: "usage" });
    await mock.set_shortener_token({ token: "t" });
    expect((await mock.get_settings()).shortener.token_ref).not.toBeNull();
    await expect(mock.shorten_url({ url: "ftp://x/y" })).rejects.toMatchObject({ code: "usage" });
    expect(await mock.shorten_url({ url })).toBe("https://bit.ly/m1");
    // save_settings keeps the token reference.
    await mock.save_settings({ settings: { ...settings, shortener: { provider: "tinyurl", token_ref: null } } });
    expect((await mock.get_settings()).shortener.token_ref).not.toBeNull();
    expect(await mock.shorten_url({ url })).toBe("https://tinyurl.com/m2");
    expect(mock.shortenedLinks()).toEqual([url, url]);
  });
});
