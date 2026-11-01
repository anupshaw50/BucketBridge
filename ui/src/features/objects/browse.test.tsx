import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { findObjectRow, openBucket, queryObjectRow, renderApp, statusBar } from "../../test/renderApp";

function visibleNames(): string[] {
  const grid = screen.getByRole("grid", { name: "Objects" });
  return within(grid)
    .queryAllByRole("row")
    .slice(1)
    .map((row) => row.querySelector(".bb-col-name span")?.textContent ?? "");
}

describe("browsing", () => {
  it("pages through a folder with next_token", async () => {
    const { user } = await renderApp({ mock: { pageSize: 3 } });
    await openBucket(user, "demo-photos");
    await findObjectRow("archive");
    expect(visibleNames()).toEqual(["archive", "documents", "emoji \u{1F4F7}"]);
    await user.click(screen.getByRole("button", { name: "Load more" }));
    await findObjectRow("readme.md");
    expect(visibleNames()).toEqual(["archive", "documents", "emoji \u{1F4F7}", "odd", "photos", "readme.md"]);
    await user.click(screen.getByRole("button", { name: "Load more" }));
    await findObjectRow("space folder");
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
    expect(visibleNames()).toEqual([
      "archive",
      "documents",
      "emoji \u{1F4F7}",
      "odd",
      "photos",
      "space folder",
      "readme.md",
    ]);
  });

  it("navigates folders by double click, Enter and breadcrumbs, showing empty key segments", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.dblClick(await findObjectRow("photos"));
    await findObjectRow("2024");
    const location = screen.getByRole("navigation", { name: "Location" });
    expect(within(location).getByRole("button", { name: "photos" })).toHaveAttribute("aria-current", "location");

    await user.click(await findObjectRow("2024"));
    expect(screen.getByRole("grid", { name: "Objects" })).toHaveFocus();
    await user.keyboard("{Enter}");
    await findObjectRow("sunset at sea.jpg");
    expect(visibleNames()).toEqual(["beach.jpg", "café.png", "sunset at sea.jpg"]);

    await user.click(within(location).getByRole("button", { name: "demo-photos" }));
    await findObjectRow("odd");
    await user.dblClick(await findObjectRow("odd"));
    await findObjectRow("(empty name)");
    await user.dblClick(await findObjectRow("(empty name)"));
    await findObjectRow("double-slash.txt");
    expect(within(location).getByRole("button", { name: "(empty name)" })).toHaveAttribute("aria-current", "location");

    expect(screen.getByRole("grid", { name: "Objects" })).toHaveFocus();
    await user.keyboard("{Backspace}");
    await findObjectRow("(empty name)");
    await user.click(screen.getByRole("button", { name: "Parent folder" }));
    await findObjectRow("readme.md");
  });

  it("selects with click, ctrl/cmd, shift, select all and invert, and shows the count", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    const readme = await findObjectRow("readme.md");
    await user.click(readme);
    expect(readme).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{Shift>}");
    await user.click(await findObjectRow("photos"));
    await user.keyboard("{/Shift}");
    const selected = () =>
      within(screen.getByRole("grid", { name: "Objects" }))
        .queryAllByRole("row", { selected: true })
        .map((row) => row.querySelector(".bb-col-name span")?.textContent);
    expect(selected()).toEqual(["photos", "space folder", "readme.md"]);
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("space folder"));
    await user.click(await findObjectRow("archive"));
    await user.keyboard("{/Control}");
    expect(selected()).toEqual(["archive", "photos", "readme.md"]);
    expect(screen.getByRole("contentinfo", { name: "Status bar" })).toHaveTextContent("7 items, 3 selected");

    const grid = screen.getByRole("grid", { name: "Objects" });
    expect(grid).toHaveFocus();
    await user.keyboard("{Control>}a{/Control}");
    expect(selected()).toHaveLength(7);
    await user.click(screen.getByRole("button", { name: "Selection" }));
    await user.click(await screen.findByRole("menuitem", { name: /Invert selection/ }));
    expect(selected()).toEqual([]);
    // The menu took focus; return to the list as a keyboard user would.
    act(() => grid.focus());
    await user.keyboard("{ArrowDown}");
    expect(selected()).toEqual(["archive"]);
    await user.keyboard("{Shift>}{ArrowDown}{ArrowDown}{/Shift}");
    expect(selected()).toEqual(["archive", "documents", "emoji \u{1F4F7}"]);
    await user.keyboard("{End}");
    expect(selected()).toEqual(["readme.md"]);
    await user.keyboard("{Escape}");
    expect(selected()).toEqual([]);
  });

  it("sorts by column, filters loaded items and switches to flat browsing", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.dblClick(await findObjectRow("documents"));
    await findObjectRow("notes.txt");
    expect(visibleNames()).toEqual(["budget 2026.xlsx", "notes.txt", "report-q3.pdf"]);
    await user.click(screen.getByRole("button", { name: "Sort by size" }));
    expect(visibleNames()).toEqual(["notes.txt", "budget 2026.xlsx", "report-q3.pdf"]);
    expect(screen.getByRole("columnheader", { name: /Size/ })).toHaveAttribute("aria-sort", "ascending");
    await user.click(screen.getByRole("button", { name: "Sort by size" }));
    expect(visibleNames()).toEqual(["report-q3.pdf", "budget 2026.xlsx", "notes.txt"]);

    await user.type(screen.getByRole("textbox", { name: "Filter loaded items" }), "NOTE");
    expect(visibleNames()).toEqual(["notes.txt"]);
    await user.clear(screen.getByRole("textbox", { name: "Filter loaded items" }));
    await user.type(screen.getByRole("textbox", { name: "Filter loaded items" }), "zzz");
    expect(await screen.findByText("No matches")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Clear filter" }));

    await user.click(screen.getByRole("button", { name: "Parent folder" }));
    await findObjectRow("readme.md");
    await user.click(screen.getByRole("button", { name: "Flat browsing" }));
    await findObjectRow("photos/2025/winter/snow.jpg");
    expect(screen.getByText("(flat view)")).toBeInTheDocument();
    expect((await mock.get_settings()).interface.flat_browsing).toBe(true);
    expect(queryObjectRow("photos")).not.toBeInTheDocument();
    // The nested folder marker comes back as an ordinary entry (is_prefix false) and is still a folder.
    const marker = await findObjectRow("photos/2025");
    expect(marker.querySelector(".bb-folder-icon")).not.toBeNull();
    expect(within(marker).getAllByRole("gridcell")[1]).toHaveTextContent("");
    await user.dblClick(marker);
    await findObjectRow("winter/snow.jpg");
    expect(screen.getByRole("navigation", { name: "Location" })).toHaveTextContent("2025");
  });

  it("shows empty and error states", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-empty").catch(() => undefined);
    expect(await screen.findByText("This folder is empty")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Upload files" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Account: Demo" }));
    await user.click(await screen.findByRole("menuitem", { name: /Wrong secret/ }));
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(/SignatureDoesNotMatch/)).toBeInTheDocument();
    expect(statusBar().getByText(/SignatureDoesNotMatch/)).toBeInTheDocument();
  });

  it("expands folders in the tree and opens them", async () => {
    const { user } = await renderApp();
    const tree = await screen.findByRole("tree", { name: "Buckets and folders" });
    const bucket = within(tree).getByText("demo-photos").closest('[role="treeitem"]');
    if (!(bucket instanceof HTMLElement)) throw new Error("no bucket node");
    expect(bucket).toHaveAttribute("aria-expanded", "false");
    act(() => bucket.focus());
    await user.keyboard("{ArrowRight}");
    await waitFor(() => expect(bucket).toHaveAttribute("aria-expanded", "true"));
    const photos = await within(tree).findByText("photos");
    await user.click(photos);
    await findObjectRow("2024");
    expect(photos.closest('[role="treeitem"]')).toHaveAttribute("aria-selected", "true");
    const odd = within(tree).getByText("odd").closest('[role="treeitem"]');
    if (!(odd instanceof HTMLElement)) throw new Error("no odd node");
    act(() => odd.focus());
    await user.keyboard("{ArrowRight}");
    const empty = await within(tree).findByText("(empty name)");
    expect(empty.closest('[role="treeitem"]')).toHaveAttribute("aria-level", "3");
    await user.keyboard("{ArrowLeft}");
    await waitFor(() => expect(within(tree).queryByText("(empty name)")).not.toBeInTheDocument());
  });
});

describe("refresh and load more", () => {
  it("does not leave load more stuck when a refresh supersedes it", async () => {
    const { user, mock } = await renderApp({ mock: { pageSize: 3 } });
    await openBucket(user, "demo-photos");
    await findObjectRow("archive");
    const original = mock.list_objects.bind(mock);
    let release: () => void = () => undefined;
    let holdNext = true;
    mock.list_objects = (args) => {
      if (holdNext && args.token !== null) {
        holdNext = false;
        return new Promise((resolve) => {
          release = () => resolve(original(args));
        });
      }
      return original(args);
    };
    await user.click(screen.getByRole("button", { name: "Load more" }));
    await user.keyboard("{F5}");
    await waitFor(() => expect(screen.getByRole("button", { name: "Load more" })).toBeEnabled());
    release();
    await user.click(screen.getByRole("button", { name: "Load more" }));
    await findObjectRow("readme.md");
    expect(visibleNames()).toEqual(["archive", "documents", "emoji \u{1F4F7}", "odd", "photos", "readme.md"]);
  });
});
