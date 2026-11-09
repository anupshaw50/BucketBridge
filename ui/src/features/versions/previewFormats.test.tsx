import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { tr } from "../../i18n";
import { findObjectRow, openBucket, renderApp } from "../../test/renderApp";

afterEach(() => {
  vi.restoreAllMocks();
});

async function preview(user: UserEvent, bucket: string, name: string): Promise<HTMLElement> {
  await openBucket(user, bucket);
  await user.click(await findObjectRow(name));
  await user.click(screen.getByRole("button", { name: tr("Preview") }));
  return screen.findByRole("dialog", { name: tr("Preview: {name}", { name }) });
}

describe("table preview", () => {
  it("shows a CSV as a table with its header and pages of 200 rows", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "sales.csv");
    const table = await within(dialog).findByRole("region", { name: tr("Contents of {name}", { name: "sales.csv" }) });
    const headers = within(table).getAllByRole("columnheader");
    expect(headers.map((cell) => cell.textContent)).toEqual(["#", "id", "region", "amount, in USD"]);
    // 40,000 bytes: 1,000 rows, five pages; the first page holds 200.
    expect(within(table).getAllByRole("row")).toHaveLength(201);
    expect(within(dialog).getByText(tr("Rows {from} to {to} of {total}", { from: "1", to: "200", total: "1,000" }))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("Page {cur} of {count}", { cur: 1, count: 5 }))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("table"))).toBeInTheDocument();
    // A null cell is marked as such, not shown as text that could be mistaken for data.
    expect(within(table).getAllByText(tr("null")).length).toBeGreaterThan(0);

    await user.click(within(dialog).getByRole("button", { name: tr("Next page") }));
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 2, count: 5 }));
    expect(within(dialog).getByText(tr("Rows {from} to {to} of {total}", { from: "201", to: "400", total: "1,000" }))).toBeInTheDocument();
    const second = within(dialog).getByRole("region", { name: tr("Contents of {name}", { name: "sales.csv" }) });
    expect(within(second).getAllByRole("row")[1]).toHaveTextContent("201");
    await user.click(within(dialog).getByRole("button", { name: tr("Last page") }));
    await within(dialog).findByText(tr("Page {cur} of {count}", { cur: 5, count: 5 }));
    expect(within(dialog).getByRole("button", { name: tr("Next page") })).toBeDisabled();
    // The byte size selector belongs to text, not to tables.
    expect(within(dialog).queryByRole("combobox", { name: tr("Page size") })).not.toBeInTheDocument();
  });

  it("shows a Parquet file the same way and says when it is too large", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "events.parquet");
    const table = await within(dialog).findByRole("region", { name: tr("Contents of {name}", { name: "events.parquet" }) });
    expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual(["#", "id", "region", "amount"]);
    expect(within(dialog).getByText(tr("Rows {from} to {to} of {total}", { from: "1", to: "200", total: "300" }))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("Page {cur} of {count}", { cur: 1, count: 2 }))).toBeInTheDocument();
  });

  it("puts cell text in the table as text, never as markup", async () => {
    const { user, mock } = await renderApp();
    const real = mock.preview_object.bind(mock);
    vi.spyOn(mock, "preview_object").mockImplementation(async (args) => {
      const answer = await real(args);
      if (answer.table === null) {
        return answer;
      }
      return { ...answer, table: { ...answer.table, rows: [['<img src=x onerror="alert(1)">', "<b>bold</b>", null]] } };
    });
    const dialog = await preview(user, "demo-logs", "sales.csv");
    const table = await within(dialog).findByRole("region", { name: tr("Contents of {name}", { name: "sales.csv" }) });
    expect(table.querySelector("img")).toBeNull();
    expect(table.querySelector("b")).toBeNull();
    expect(within(table).getByText('<img src=x onerror="alert(1)">')).toBeInTheDocument();
    expect(within(table).getByText("<b>bold</b>")).toBeInTheDocument();
  });

  it("warns when only the start of a large file is shown and when columns are cut", async () => {
    const { user, mock } = await renderApp();
    const real = mock.preview_object.bind(mock);
    vi.spyOn(mock, "preview_object").mockImplementation(async (args) => {
      const answer = await real(args);
      return answer.table === null ? answer : { ...answer, table: { ...answer.table, truncated: true, columns_cut: true } };
    });
    const dialog = await preview(user, "demo-logs", "sales.csv");
    expect(
      await within(dialog).findByText(tr("This file is larger than the preview limit; only its first {rows} rows are shown.", { rows: "1,000" })),
    ).toBeInTheDocument();
    expect(within(dialog).getByText(tr("Only the first {count} columns are shown.", { count: "3" }))).toBeInTheDocument();
  });
});

describe("XML and GIF previews", () => {
  it("shows XML re-indented and labelled as formatted", async () => {
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "catalog.xml");
    const contents = await within(dialog).findByLabelText(tr("Contents of {name}", { name: "catalog.xml" }));
    expect(contents.tagName).toBe("PRE");
    expect(contents.textContent).toContain("<catalog>\n  <book id=\"1\">\n    <title>First</title>");
    expect(within(dialog).getByText(tr("XML (formatted)"))).toBeInTheDocument();
  });

  it("draws a GIF with an img element so that it plays", async () => {
    const create = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:preview-gif");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "spinner.gif");
    const image = await within(dialog).findByRole("img", { name: tr("Preview of {name}", { name: "spinner.gif" }) });
    expect(image.tagName).toBe("IMG");
    expect(image).toHaveAttribute("src", "blob:preview-gif");
    expect((create.mock.calls[0]?.[0] as Blob).type).toBe("image/gif");
  });
});

describe("zooming and panning an image", () => {
  async function open() {
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:preview-zoom");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    const { user } = await renderApp();
    const dialog = await preview(user, "demo-logs", "monitoring-dashboard.webp");
    const image = await within(dialog).findByRole("img", { name: tr("Preview of {name}", { name: "monitoring-dashboard.webp" }) });
    return { user, dialog, image };
  }
  const level = (dialog: HTMLElement) => within(dialog).getByLabelText(tr("Zoom level"));

  it("starts fitted and zooms with the buttons, 100 % and back to fit", async () => {
    const { user, dialog, image } = await open();
    expect(level(dialog)).toHaveTextContent(tr("Fit"));
    expect(image.style.maxWidth).toBe("100%");
    await user.click(within(dialog).getByRole("button", { name: tr("Zoom in") }));
    expect(level(dialog)).toHaveTextContent("125%");
    expect(image.style.transform).toContain("scale(1.25)");
    await user.click(within(dialog).getByRole("button", { name: tr("Zoom out") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Zoom out") }));
    expect(level(dialog)).toHaveTextContent("80%");
    await user.click(within(dialog).getByRole("button", { name: tr("Actual size") }));
    expect(level(dialog)).toHaveTextContent("100%");
    expect(image.style.maxWidth).toBe("none");
    await user.click(within(dialog).getByRole("button", { name: tr("Fit to window") }));
    expect(level(dialog)).toHaveTextContent(tr("Fit"));
    expect(image.style.maxWidth).toBe("100%");
  });

  it("zooms with the wheel, with a trackpad pinch and with the keyboard", async () => {
    const { user, dialog } = await open();
    const frame = within(dialog).getByRole("group", { name: /Image viewer|图片查看器/ });
    fireEvent.wheel(frame, { deltaY: -100 });
    await waitFor(() => expect(level(dialog)).not.toHaveTextContent(tr("Fit")));
    const afterWheel = level(dialog).textContent;
    expect(Number.parseInt(afterWheel, 10)).toBeGreaterThan(100);
    fireEvent.wheel(frame, { deltaY: -20, ctrlKey: true });
    expect(Number.parseInt(level(dialog).textContent, 10)).toBeGreaterThan(Number.parseInt(afterWheel, 10));
    frame.focus();
    await user.keyboard("1");
    expect(level(dialog)).toHaveTextContent("100%");
    await user.keyboard("+");
    expect(level(dialog)).toHaveTextContent("125%");
    await user.keyboard("-");
    expect(level(dialog)).toHaveTextContent("100%");
    await user.keyboard("0");
    expect(level(dialog)).toHaveTextContent(tr("Fit"));
  });

  it("pans with the arrow keys and double click toggles fit and 100 %", async () => {
    const { user, dialog, image } = await open();
    const frame = within(dialog).getByRole("group", { name: /Image viewer|图片查看器/ });
    frame.focus();
    await user.keyboard("1");
    const before = image.style.transform;
    await user.keyboard("{ArrowLeft}");
    // jsdom has no layout, so the pan is not clamped: the picture moved by one step to the right.
    expect(image.style.transform).not.toBe(before);
    expect(image.style.transform).toContain("translate(40px, 0px)");
    await user.keyboard("{ArrowDown}");
    expect(image.style.transform).toContain("translate(40px, -40px)");
    await user.keyboard("0");
    await user.dblClick(frame);
    expect(level(dialog)).toHaveTextContent("100%");
    await user.dblClick(frame);
    expect(level(dialog)).toHaveTextContent(tr("Fit"));
  });
});
