import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { renderApp } from "../../test/renderApp";

async function openWizard(user: UserEvent): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
  const row = (await within(tree).findByText("demo-photos")).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error("no tree row");
  }
  fireEvent.contextMenu(row);
  await user.click(screen.getByRole("menuitem", { name: tr("Sharing wizard…") }));
  return screen.findByRole("dialog", { name: tr("Sharing wizard: {bucket}", { bucket: "demo-photos" }) });
}

async function toAccessStep(user: UserEvent, dialog: HTMLElement, key: string): Promise<void> {
  await user.type(within(dialog).getByRole("combobox", { name: tr("Object key") }), key);
  await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
}

describe("sharing wizard: public links and snippets", () => {
  it("builds a link on a CloudFront or custom host, with image snippets", async () => {
    const { user } = await renderApp();
    const dialog = await openWizard(user);
    await toAccessStep(user, dialog, "pets/cat 1.png");
    await user.click(within(dialog).getByRole("radio", { name: tr("Public link (CloudFront or your host name)") }));
    expect(within(dialog).queryByRole("textbox", { name: tr("Valid for") })).toBeNull();

    await user.click(within(dialog).getByRole("button", { name: tr("Create link") }));
    expect(await within(dialog).findByText(tr("Enter the host name of the CloudFront distribution or of your site."))).toBeInTheDocument();
    await user.type(within(dialog).getByRole("combobox", { name: tr("Host name") }), "cdn.example.com/x");
    await user.click(within(dialog).getByRole("button", { name: tr("Create link") }));
    expect(await within(dialog).findByText(tr("Enter a host name such as cdn.example.com, without a path."))).toBeInTheDocument();

    const host = within(dialog).getByRole("combobox", { name: tr("Host name") });
    await user.clear(host);
    await user.type(host, "https://CDN.example.com/");
    await user.click(within(dialog).getByRole("button", { name: tr("Create link") }));
    const link = await within(dialog).findByRole<HTMLTextAreaElement>("textbox", { name: tr("Link") });
    expect(link.value).toBe("https://cdn.example.com/pets/cat%201.png");
    expect(within(dialog).getByRole<HTMLTextAreaElement>("textbox", { name: tr("HTML") }).value).toBe('<img src="https://cdn.example.com/pets/cat%201.png" alt="cat 1.png">');
    expect(within(dialog).getByRole<HTMLTextAreaElement>("textbox", { name: tr("Markdown") }).value).toBe("![cat 1.png](https://cdn.example.com/pets/cat%201.png)");
  });

  it("offers the distributions of the bucket to pick the host from", async () => {
    const { user } = await renderApp();
    const dialog = await openWizard(user);
    await toAccessStep(user, dialog, "readme.md");
    await user.click(within(dialog).getByRole("radio", { name: tr("Public link (CloudFront or your host name)") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Find distributions") }));
    await user.click(within(dialog).getByRole("combobox", { name: tr("Host name") }));
    const option = await screen.findByRole("option", { name: /cloudfront\.net/ });
    await user.click(option);
    await user.click(within(dialog).getByRole("button", { name: tr("Create link") }));
    const link = await within(dialog).findByRole<HTMLTextAreaElement>("textbox", { name: tr("Link") });
    expect(link.value).toMatch(/^https:\/\/[a-z0-9.-]+\.cloudfront\.net\/readme\.md$/);
    // A document is a text link, not an image.
    expect(within(dialog).getByRole<HTMLTextAreaElement>("textbox", { name: tr("HTML") }).value).toContain('<a href="https://');
    expect(within(dialog).getByRole<HTMLTextAreaElement>("textbox", { name: tr("Markdown") }).value).toMatch(/^\[readme\.md\]\(https:/);
  });

  it("adds snippets to a signed link too", async () => {
    const { user } = await renderApp();
    const dialog = await openWizard(user);
    await toAccessStep(user, dialog, "readme.md");
    await user.click(within(dialog).getByRole("button", { name: tr("Create link") }));
    await within(dialog).findByRole("textbox", { name: tr("Link") });
    expect(within(dialog).getByRole<HTMLTextAreaElement>("textbox", { name: tr("HTML") }).value).toContain("X-Amz-Signature=");
  });

  it("lets the user pick the shortener for one link", async () => {
    const { user, mock } = await renderApp({
      prepare: async (backend) => {
        const settings = await backend.get_settings();
        settings.shortener.provider = "bitly";
        await backend.save_settings({ settings });
        await backend.set_shortener_token({ token: "t" });
      },
    });
    const dialog = await openWizard(user);
    await toAccessStep(user, dialog, "readme.md");
    await user.click(within(dialog).getByRole("button", { name: tr("Create link") }));
    await within(dialog).findByRole("textbox", { name: tr("Link") });
    await user.click(within(dialog).getByRole("combobox", { name: tr("Service") }));
    await user.click(await screen.findByRole("option", { name: "TinyURL" }));
    expect(within(dialog).getByText(tr("Shorten sends the whole link, including its signature, to {name}, outside this machine.", { name: "TinyURL" }))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Shorten") }));
    const short = await within(dialog).findByRole<HTMLInputElement>("textbox", { name: tr("Short link") });
    expect(short.value).toBe("https://tinyurl.com/m1");
    await waitFor(() => expect(mock.shortenedLinks()).toHaveLength(1));
  });
});
