import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BackendError } from "../../api/errors";
import { tr } from "../../i18n";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";

/** Escapes a string for use in a RegExp. */
const esc = (text: string): string => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

afterEach(() => {
  vi.restoreAllMocks();
});

/** Selects an object of a demo bucket. */
async function select(user: UserEvent, bucket: string, name: string): Promise<void> {
  await openBucket(user, bucket);
  await user.click(await findObjectRow(name));
}

async function preview(user: UserEvent, bucket: string, name: string): Promise<HTMLElement> {
  await select(user, bucket, name);
  await user.click(screen.getByRole("button", { name: tr("Preview") }));
  return screen.findByRole("dialog", { name: tr("Preview: {name}", { name }) });
}

describe("opening the preview", () => {
  it("is a toolbar button for one object, and F3 in the list", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-logs");
    const button = screen.getByRole("button", { name: tr("Preview") });
    expect(button).toBeDisabled();
    // Not for a folder.
    await user.click(await findObjectRow("app"));
    expect(button).toBeDisabled();
    await user.keyboard("{F3}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    // Not for two objects.
    await user.click(await findObjectRow("latest.log"));
    expect(button).toBeEnabled();
    await user.keyboard("{Control>}{a}{/Control}");
    expect(button).toBeDisabled();
    // F3 on one object.
    await user.click(await findObjectRow("latest.log"));
    await user.keyboard("{F3}");
    const dialog = await screen.findByRole("dialog", { name: tr("Preview: {name}", { name: "latest.log" }) });
    expect(await within(dialog).findByLabelText(tr("Contents of {name}", { name: "latest.log" }))).toHaveTextContent("line 0000000");
    // No version badge for the current version.
    expect(within(dialog).queryByText(new RegExp(`^${esc(tr("Version {id}", { id: "" }))}`))).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });
});

describe("what a preview shows", () => {
  it("shows text as text, never as markup, with its details", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "latest.log");
    const contents = await within(dialog).findByLabelText(tr("Contents of {name}", { name: "latest.log" }));
    expect(contents.tagName).toBe("PRE");
    expect(contents).toHaveTextContent("latest.log:");
    expect(within(dialog).getByText(tr("text"))).toBeInTheDocument();
    expect(within(dialog).getByText(/12\.0 KiB/)).toBeInTheDocument();
    // One page: the paging controls are there but have nowhere to go.
    expect(within(dialog).getByText(tr("Page {cur} of {count}", { cur: 1, count: 1 }))).toBeInTheDocument();
    for (const name of ["First page", "Previous page", "Next page", "Last page"]) {
      expect(within(dialog).getByRole("button", { name: tr(name) })).toBeDisabled();
    }
  });

  it("pretty-prints JSON that fits a page", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "config.json");
    const contents = await within(dialog).findByLabelText(tr("Contents of {name}", { name: "config.json" }));
    expect(within(dialog).getByText(tr("JSON (formatted)"))).toBeInTheDocument();
    expect(contents.textContent).toContain('{\n  "name": "config.json",');
    expect(contents.textContent).toContain('"tags": [\n    "alpha",\n    "beta"\n  ]');
  });

  it("draws an image from a blob URL of the right type and releases it", async () => {
    const create = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:preview-webp");
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "monitoring-dashboard.webp");
    const image = await within(dialog).findByRole("img", { name: tr("Preview of {name}", { name: "monitoring-dashboard.webp" }) });
    expect(image).toHaveAttribute("src", "blob:preview-webp");
    const blob = create.mock.calls[0]?.[0];
    expect(blob).toBeInstanceOf(Blob);
    expect((blob as Blob).type).toBe("image/webp");
    expect(within(dialog).queryByLabelText(new RegExp(`^${esc(tr("Contents of {name}", { name: "" }))}`))).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(revoke).toHaveBeenCalledWith("blob:preview-webp"));
  });

  it("draws other images too, and says so when the web view cannot", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-photos", "readme.md").catch(() => null);
    expect(dialog).not.toBeNull();
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: tr("Close") }));
    await user.click(await findObjectRow("photos"));
    await user.keyboard("{Enter}");
    await user.click(await findObjectRow("2024"));
    await user.keyboard("{Enter}");
    await user.click(await findObjectRow("beach.jpg"));
    await user.keyboard("{F3}");
    const picture = await screen.findByRole("dialog", { name: tr("Preview: {name}", { name: "beach.jpg" }) });
    const image = await within(picture).findByRole("img", { name: tr("Preview of {name}", { name: "beach.jpg" }) });
    expect(image.getAttribute("src")).toMatch(/^(blob:|data:image\/png;base64,)/);
    // The web view fails to decode it.
    fireEvent.error(image);
    expect(await within(picture).findByText(tr("The image cannot be shown"))).toBeInTheDocument();
    expect(within(picture).queryByRole("img")).not.toBeInTheDocument();
  });

  it("says why there is nothing to show: too large, binary, empty", async () => {
    const { user } = await renderApp();
    const large = await preview(user, "demo-logs", "site-photo-large.png");
    expect(await within(large).findByText(tr("Too large to preview"))).toBeInTheDocument();
    expect(
      within(large).getByText(
        tr("This {type} is {size}. Images above 8 MiB are not previewed; download the object to open it.", {
          type: "image/png",
          size: "24.0 MiB",
        }),
      ),
    ).toBeInTheDocument();
    expect(within(large).queryByRole("img")).not.toBeInTheDocument();
    await user.click(within(large).getByRole("button", { name: tr("Close") }));

    await user.click(await findObjectRow("snapshot.bin"));
    await user.keyboard("{F3}");
    const binary = await screen.findByRole("dialog", { name: tr("Preview: {name}", { name: "snapshot.bin" }) });
    expect(await within(binary).findByText(tr("Binary data"))).toBeInTheDocument();
    expect(
      within(binary).getByText(
        tr(
          "This object holds data that is neither text nor an image ({size}{type}). Download it to open it with another program.",
          { size: "2.0 MiB", type: "" },
        ),
      ),
    ).toBeInTheDocument();
    await user.click(within(binary).getByRole("button", { name: tr("Close") }));

    await user.click(await findObjectRow("empty.txt"));
    await user.keyboard("{F3}");
    const empty = await screen.findByRole("dialog", { name: tr("Preview: {name}", { name: "empty.txt" }) });
    expect(await within(empty).findByText(tr("Empty object"))).toBeInTheDocument();
    expect(within(empty).getByText(tr("This object has no content."))).toBeInTheDocument();
  });

  it("shows the error when the preview cannot be loaded", async () => {
    const { user, mock } = await renderApp();
    vi.spyOn(mock, "preview_object").mockRejectedValueOnce(new BackendError("not_found", "The specified key does not exist (NoSuchKey)."));
    await select(user, "demo-logs", "latest.log");
    await user.keyboard("{F3}");
    const dialog = await screen.findByRole("dialog", { name: tr("Preview: {name}", { name: "latest.log" }) });
    expect(await within(dialog).findByText(tr("The preview could not be loaded"))).toBeInTheDocument();
    expect(within(dialog).getByText("The specified key does not exist (NoSuchKey).")).toBeInTheDocument();
  });
});

describe("paging through long text", () => {
  it("goes to the first, previous, next and last page and to a page number", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "audit-export.log");
    const contents = await within(dialog).findByLabelText(tr("Contents of {name}", { name: "audit-export.log" }));
    expect(contents).toHaveTextContent("line 0000000");
    expect(within(dialog).getByText(tr("Page {cur} of {count}", { cur: 1, count: 5 }))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("Bytes {from} to {to}", { from: "0", to: "131,071" }))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("First page") })).toBeDisabled();
    expect(within(dialog).getByRole("button", { name: tr("Previous page") })).toBeDisabled();

    await user.click(within(dialog).getByRole("button", { name: tr("Next page") }));
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 2, count: 5 }));
    // 131,072 bytes of 64-byte lines: the second page starts at line 2048.
    expect(within(dialog).getByLabelText(tr("Contents of {name}", { name: "audit-export.log" }))).toHaveTextContent("line 0002048");
    expect(within(dialog).getByText(tr("Bytes {from} to {to}", { from: "131,072", to: "262,143" }))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("First page") })).toBeEnabled();

    await user.click(within(dialog).getByRole("button", { name: tr("Last page") }));
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 5, count: 5 }));
    expect(within(dialog).getByText(tr("Bytes {from} to {to}", { from: "524,288", to: "614,399" }))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Next page") })).toBeDisabled();
    expect(within(dialog).getByRole("button", { name: tr("Last page") })).toBeDisabled();

    await user.click(within(dialog).getByRole("button", { name: tr("Previous page") }));
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 4, count: 5 }));
    await user.click(within(dialog).getByRole("button", { name: tr("First page") }));
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 1, count: 5 }));

    // A page number, with the button or Enter; one outside the range is brought into it.
    const number = within(dialog).getByRole("textbox", { name: tr("Page number") });
    await user.clear(number);
    await user.type(number, "3");
    await user.click(within(dialog).getByRole("button", { name: tr("Go") }));
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 3, count: 5 }));
    const again = within(dialog).getByRole("textbox", { name: tr("Page number") });
    await user.clear(again);
    await user.type(again, "9{Enter}");
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 5, count: 5 }));
  });

  it("pages with Alt and the arrow keys", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "audit-export.log");
    const contents = await within(dialog).findByLabelText(tr("Contents of {name}", { name: "audit-export.log" }));
    contents.focus();
    await user.keyboard("{Alt>}{ArrowRight}{/Alt}");
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 2, count: 5 }));
    await user.keyboard("{Alt>}{End}{/Alt}");
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 5, count: 5 }));
    await user.keyboard("{Alt>}{ArrowLeft}{/Alt}");
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 4, count: 5 }));
    await user.keyboard("{Alt>}{Home}{/Alt}");
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 1, count: 5 }));
    // Without Alt the keys keep their meaning (scrolling the text).
    await user.keyboard("{ArrowRight}");
    expect(within(dialog).getByText(tr("Page {cur} of {count}", { cur: 1, count: 5 }))).toBeInTheDocument();
  });

  it("changes the page size with Apply and starts again at the first page", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "audit-export.log");
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 1, count: 5 }));
    await user.click(within(dialog).getByRole("button", { name: tr("Next page") }));
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 2, count: 5 }));
    await user.click(within(dialog).getByRole("combobox", { name: tr("Page size") }));
    await user.click(await screen.findByRole("option", { name: "1 MiB" }));
    // Nothing changes until Apply.
    expect(within(dialog).getByText(tr("Page {cur} of {count}", { cur: 2, count: 5 }))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Apply") }));
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 1, count: 1 }));
    expect(within(dialog).getByText(tr("Bytes {from} to {to}", { from: "0", to: "614,399" }))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Next page") })).toBeDisabled();
  });
});
