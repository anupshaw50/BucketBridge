import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BackendError } from "../../api/errors";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";

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
  await user.click(screen.getByRole("button", { name: "Preview" }));
  return screen.findByRole("dialog", { name: new RegExp(`Preview: ${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`) });
}

describe("opening the preview", () => {
  it("is a toolbar button for one object, and F3 in the list", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-logs");
    const button = screen.getByRole("button", { name: "Preview" });
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
    const dialog = await screen.findByRole("dialog", { name: /Preview: latest\.log/ });
    expect(await within(dialog).findByLabelText("Contents of latest.log")).toHaveTextContent("line 0000000");
    // No version badge for the current version.
    expect(within(dialog).queryByText(/^Version /)).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });
});

describe("what a preview shows", () => {
  it("shows text as text, never as markup, with its details", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "latest.log");
    const contents = await within(dialog).findByLabelText("Contents of latest.log");
    expect(contents.tagName).toBe("PRE");
    expect(contents).toHaveTextContent("latest.log:");
    expect(within(dialog).getByText("text")).toBeInTheDocument();
    expect(within(dialog).getByText(/12\.0 KiB/)).toBeInTheDocument();
    // One page: the paging controls are there but have nowhere to go.
    expect(within(dialog).getByText("Page 1 of 1")).toBeInTheDocument();
    for (const name of ["First page", "Previous page", "Next page", "Last page"]) {
      expect(within(dialog).getByRole("button", { name })).toBeDisabled();
    }
  });

  it("pretty-prints JSON that fits a page", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "config.json");
    const contents = await within(dialog).findByLabelText("Contents of config.json");
    expect(within(dialog).getByText("JSON (formatted)")).toBeInTheDocument();
    expect(contents.textContent).toContain('{\n  "name": "config.json",');
    expect(contents.textContent).toContain('"tags": [\n    "alpha",\n    "beta"\n  ]');
  });

  it("draws an image from a blob URL of the right type and releases it", async () => {
    const create = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:preview-webp");
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "monitoring-dashboard.webp");
    const image = await within(dialog).findByRole("img", { name: "Preview of monitoring-dashboard.webp" });
    expect(image).toHaveAttribute("src", "blob:preview-webp");
    const blob = create.mock.calls[0]?.[0];
    expect(blob).toBeInstanceOf(Blob);
    expect((blob as Blob).type).toBe("image/webp");
    expect(within(dialog).queryByLabelText(/^Contents of/)).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(revoke).toHaveBeenCalledWith("blob:preview-webp"));
  });

  it("draws other images too, and says so when the web view cannot", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-photos", "readme.md").catch(() => null);
    expect(dialog).not.toBeNull();
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Close" }));
    await user.click(await findObjectRow("photos"));
    await user.keyboard("{Enter}");
    await user.click(await findObjectRow("2024"));
    await user.keyboard("{Enter}");
    await user.click(await findObjectRow("beach.jpg"));
    await user.keyboard("{F3}");
    const picture = await screen.findByRole("dialog", { name: /Preview: beach\.jpg/ });
    const image = await within(picture).findByRole("img", { name: "Preview of beach.jpg" });
    expect(image.getAttribute("src")).toMatch(/^(blob:|data:image\/png;base64,)/);
    // The web view fails to decode it.
    fireEvent.error(image);
    expect(await within(picture).findByText("The image cannot be shown")).toBeInTheDocument();
    expect(within(picture).queryByRole("img")).not.toBeInTheDocument();
  });

  it("says why there is nothing to show: too large, binary, empty", async () => {
    const { user } = await renderApp();
    const large = await preview(user, "demo-logs", "site-photo-large.png");
    expect(await within(large).findByText("Too large to preview")).toBeInTheDocument();
    expect(within(large).getByText(/24\.0 MiB\. Images above 8 MiB are not previewed/)).toBeInTheDocument();
    expect(within(large).queryByRole("img")).not.toBeInTheDocument();
    await user.click(within(large).getByRole("button", { name: "Close" }));

    await user.click(await findObjectRow("snapshot.bin"));
    await user.keyboard("{F3}");
    const binary = await screen.findByRole("dialog", { name: /Preview: snapshot\.bin/ });
    expect(await within(binary).findByText("Binary data")).toBeInTheDocument();
    expect(within(binary).getByText(/neither text nor an image \(2\.0 MiB\)/)).toBeInTheDocument();
    await user.click(within(binary).getByRole("button", { name: "Close" }));

    await user.click(await findObjectRow("empty.txt"));
    await user.keyboard("{F3}");
    const empty = await screen.findByRole("dialog", { name: /Preview: empty\.txt/ });
    expect(await within(empty).findByText("Empty object")).toBeInTheDocument();
    expect(within(empty).getByText("This object has no content.")).toBeInTheDocument();
  });

  it("shows the error when the preview cannot be loaded", async () => {
    const { user, mock } = await renderApp();
    vi.spyOn(mock, "preview_object").mockRejectedValueOnce(new BackendError("not_found", "The specified key does not exist (NoSuchKey)."));
    await select(user, "demo-logs", "latest.log");
    await user.keyboard("{F3}");
    const dialog = await screen.findByRole("dialog", { name: /Preview: latest\.log/ });
    expect(await within(dialog).findByText("The preview could not be loaded")).toBeInTheDocument();
    expect(within(dialog).getByText("The specified key does not exist (NoSuchKey).")).toBeInTheDocument();
  });
});

describe("paging through long text", () => {
  it("goes to the first, previous, next and last page and to a page number", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "audit-export.csv");
    const contents = await within(dialog).findByLabelText("Contents of audit-export.csv");
    expect(contents).toHaveTextContent("line 0000000");
    expect(within(dialog).getByText("Page 1 of 5")).toBeInTheDocument();
    expect(within(dialog).getByText("Bytes 0 to 131,071")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "First page" })).toBeDisabled();
    expect(within(dialog).getByRole("button", { name: "Previous page" })).toBeDisabled();

    await user.click(within(dialog).getByRole("button", { name: "Next page" }));
    await within(dialog).findByText("Page 2 of 5");
    // 131,072 bytes of 64-byte lines: the second page starts at line 2048.
    expect(within(dialog).getByLabelText("Contents of audit-export.csv")).toHaveTextContent("line 0002048");
    expect(within(dialog).getByText("Bytes 131,072 to 262,143")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "First page" })).toBeEnabled();

    await user.click(within(dialog).getByRole("button", { name: "Last page" }));
    await within(dialog).findByText("Page 5 of 5");
    expect(within(dialog).getByText("Bytes 524,288 to 614,399")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Next page" })).toBeDisabled();
    expect(within(dialog).getByRole("button", { name: "Last page" })).toBeDisabled();

    await user.click(within(dialog).getByRole("button", { name: "Previous page" }));
    await within(dialog).findByText("Page 4 of 5");
    await user.click(within(dialog).getByRole("button", { name: "First page" }));
    await within(dialog).findByText("Page 1 of 5");

    // A page number, with the button or Enter; one outside the range is brought into it.
    const number = within(dialog).getByRole("textbox", { name: "Page number" });
    await user.clear(number);
    await user.type(number, "3");
    await user.click(within(dialog).getByRole("button", { name: "Go" }));
    await within(dialog).findByText("Page 3 of 5");
    const again = within(dialog).getByRole("textbox", { name: "Page number" });
    await user.clear(again);
    await user.type(again, "9{Enter}");
    await within(dialog).findByText("Page 5 of 5");
  });

  it("pages with Alt and the arrow keys", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "audit-export.csv");
    const contents = await within(dialog).findByLabelText("Contents of audit-export.csv");
    contents.focus();
    await user.keyboard("{Alt>}{ArrowRight}{/Alt}");
    await within(dialog).findByText("Page 2 of 5");
    await user.keyboard("{Alt>}{End}{/Alt}");
    await within(dialog).findByText("Page 5 of 5");
    await user.keyboard("{Alt>}{ArrowLeft}{/Alt}");
    await within(dialog).findByText("Page 4 of 5");
    await user.keyboard("{Alt>}{Home}{/Alt}");
    await within(dialog).findByText("Page 1 of 5");
    // Without Alt the keys keep their meaning (scrolling the text).
    await user.keyboard("{ArrowRight}");
    expect(within(dialog).getByText("Page 1 of 5")).toBeInTheDocument();
  });

  it("changes the page size with Apply and starts again at the first page", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "audit-export.csv");
    await within(dialog).findByText("Page 1 of 5");
    await user.click(within(dialog).getByRole("button", { name: "Next page" }));
    await within(dialog).findByText("Page 2 of 5");
    await user.click(within(dialog).getByRole("combobox", { name: "Page size" }));
    await user.click(await screen.findByRole("option", { name: "1 MiB" }));
    // Nothing changes until Apply.
    expect(within(dialog).getByText("Page 2 of 5")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Apply" }));
    await within(dialog).findByText("Page 1 of 1");
    expect(within(dialog).getByText("Bytes 0 to 614,399")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Next page" })).toBeDisabled();
  });
});
