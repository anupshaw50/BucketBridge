import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { AVAILABLE } from "../menus/available";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";
import { applicationName, redirectProblem } from "./helpers";

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: "Buckets and folders" });
  const row = (await within(tree).findByText(name)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${name}`);
  }
  return row;
}

describe("file open helpers", () => {
  it("names applications and checks redirect locations", () => {
    expect(applicationName("/Applications/Text Editor.app")).toBe("Text Editor");
    expect(applicationName("C:\\Tools\\viewer.exe")).toBe("viewer");
    expect(applicationName("/usr/bin/gimp")).toBe("gimp");
    expect(redirectProblem("")).toBeNull();
    expect(redirectProblem("/a.html")).toBeNull();
    expect(redirectProblem("https://example.com/")).toBeNull();
    expect(redirectProblem("example.com")).not.toBeNull();
  });

  it("turns the three menu entries on", () => {
    expect([AVAILABLE["file.open-with"], AVAILABLE["file.website-redirect"], AVAILABLE["bucket.open-browser"]]).toEqual([true, true, true]);
  });
});

describe("Open with", () => {
  it("opens the object with a picked application and lists it as recent next time", async () => {
    const { user, mock, platform } = await renderApp();
    await openBucket(user, "demo-photos");
    platform.applicationAnswers.push("/Applications/Text Editor.app");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(await screen.findByRole("menuitem", { name: /^Open with/ }));
    let dialog = await screen.findByRole("dialog", { name: "Open with" });
    expect(within(dialog).queryByText("Recent applications")).toBeNull();
    await user.click(within(dialog).getByRole("button", { name: "Choose application…" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Open with" })).toBeNull());
    expect((await mock.get_settings()).general.open_with_recent).toEqual(["/Applications/Text Editor.app"]);

    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(await screen.findByRole("menuitem", { name: /^Open with/ }));
    dialog = await screen.findByRole("dialog", { name: "Open with" });
    await user.click(within(dialog).getByRole("button", { name: "Text Editor" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Open with" })).toBeNull());
    expect((await mock.get_settings()).general.open_with_recent).toEqual(["/Applications/Text Editor.app"]);
  });

  it("does nothing when the picker is cancelled", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    fireEvent.contextMenu(await findObjectRow("readme.md"));
    await user.click(await screen.findByRole("menuitem", { name: /^Open with/ }));
    const dialog = await screen.findByRole("dialog", { name: "Open with" });
    await user.click(within(dialog).getByRole("button", { name: "Choose application…" }));
    expect(screen.getByRole("dialog", { name: "Open with" })).toBeInTheDocument();
    expect((await mock.get_settings()).general.open_with_recent).toEqual([]);
  });
});

describe("Website redirect", () => {
  it("sets, shows and clears the redirect location", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    const read = async () => {
      const list = await mock.list_accounts();
      return (
        await mock.get_object_details({ account_id: list.default_account_id ?? "", bucket: "demo-photos", key: "readme.md", sse_c_key: null })
      ).meta.website_redirect;
    };
    const open = async () => {
      fireEvent.contextMenu(await findObjectRow("readme.md"));
      await user.click(await screen.findByRole("menuitem", { name: "Metadata" }));
      await user.click(within(await screen.findByRole("menu", { name: "Metadata" })).getByRole("menuitem", { name: /^Website redirect/ }));
      return screen.findByRole("dialog", { name: "Website redirect" });
    };

    let dialog = await open();
    const input = within(dialog).getByRole("textbox", { name: "Redirect location" });
    const save = within(dialog).getByRole("button", { name: "Save" });
    expect(save).toBeDisabled();
    await user.type(input, "example.com");
    expect(within(dialog).getByText(/Use a path that starts with/)).toBeInTheDocument();
    expect(save).toBeDisabled();
    await user.clear(input);
    await user.type(input, "/other.html");
    await user.click(save);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Website redirect" })).toBeNull());
    expect(await read()).toBe("/other.html");

    dialog = await open();
    await waitFor(() => expect(within(dialog).getByRole("textbox", { name: "Redirect location" })).toHaveValue("/other.html"));
    await user.clear(within(dialog).getByRole("textbox", { name: "Redirect location" }));
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Website redirect" })).toBeNull());
    expect(await read()).toBeNull();
  });
});

describe("Open in browser", () => {
  it("opens the bucket address through the backend", async () => {
    const { user, mock } = await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(await screen.findByRole("menuitem", { name: /^Open in browser/ }));
    await waitFor(() => expect(mock.openedUrls).toHaveLength(1));
    expect(mock.openedUrls[0]).toMatch(/^https?:\/\/.*demo-photos/);
  });
});
