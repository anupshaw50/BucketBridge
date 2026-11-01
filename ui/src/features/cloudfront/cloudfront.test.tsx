import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { UserEvent } from "@testing-library/user-event";
import { renderApp } from "../../test/renderApp";
import { MAX_INVALIDATION_PATHS, pathsFromText, validatePaths } from "./paths";

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: "Buckets and folders" });
  const row = (await within(tree).findByText(name)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${name}`);
  }
  return row;
}

/** Opens the manager from the empty area of the buckets pane. */
async function openManager(user: UserEvent): Promise<void> {
  const pane = document.querySelector(".bb-tree-pane");
  expect(pane).not.toBeNull();
  if (pane !== null) {
    fireEvent.contextMenu(pane);
  }
  await user.click(await screen.findByRole("menuitem", { name: "CloudFront manager…" }));
}

describe("invalidation paths", () => {
  it("accepts wildcards only at the end and trims and deduplicates", () => {
    expect(validatePaths(["/*", " /img/* ", "/a.html", "/a.html"])).toEqual({ ok: true, paths: ["/*", "/img/*", "/a.html"] });
    for (const bad of ["/a*b", "/*/x", "a", "", "/a b", "/a\tb"]) {
      expect(validatePaths([bad]).ok, bad).toBe(false);
    }
  });

  it("reports every problem and the limits", () => {
    const result = validatePaths(["a", "/b*c"]);
    expect(result.ok).toBe(false);
    expect(result.ok ? "" : result.message).toContain("must start with '/'");
    expect(result.ok ? "" : result.message).toContain("only allowed as the last");
    expect(validatePaths([]).ok).toBe(false);
    const many = Array.from({ length: MAX_INVALIDATION_PATHS + 1 }, (_, i) => `/${i}`);
    expect(validatePaths(many).ok).toBe(false);
    expect(validatePaths(many.slice(0, MAX_INVALIDATION_PATHS)).ok).toBe(true);
  });

  it("splits a text box into lines", () => {
    expect(pathsFromText("/a\r\n\n  /b  \n")).toEqual(["/a", "/b"]);
  });
});

describe("the CloudFront manager", () => {
  it("lists distributions from the buckets pane menu and enables a disabled one after a confirmation", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await openManager(user);
    const dialog = await screen.findByRole("dialog", { name: "CloudFront manager" });
    await within(dialog).findByRole("row", { name: "Distribution EDEMO0001" });
    expect(within(dialog).queryByRole("row", { name: "Distribution EDEMO0003" })).toBeNull();
    await user.click(within(dialog).getByRole("button", { name: "Load more" }));
    await within(dialog).findByRole("row", { name: "Distribution EDEMO0003" });
    const other = within(dialog).getByRole("row", { name: "Distribution EDEMO0002" });
    expect(within(other).getByText("Disabled")).toBeInTheDocument();
    await user.click(within(other).getByRole("button", { name: "Enable EDEMO0002" }));
    const confirm = await screen.findByRole("dialog", { name: "Enable distribution" });
    await user.click(within(confirm).getByRole("button", { name: "Enable" }));
    await waitFor(() => expect(within(within(dialog).getByRole("row", { name: "Distribution EDEMO0002" })).getByText("Enabled")).toBeInTheDocument());
  });

  it("asks for the id before disabling a distribution", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await openManager(user);
    const dialog = await screen.findByRole("dialog", { name: "CloudFront manager" });
    const row = await within(dialog).findByRole("row", { name: "Distribution EDEMO0001" });
    await user.click(within(row).getByRole("button", { name: "Disable EDEMO0001" }));
    const confirm = await screen.findByRole("dialog", { name: "Disable distribution" });
    expect(within(confirm).getByRole("button", { name: "Disable" })).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "EDEMO0001");
    await user.click(within(confirm).getByRole("button", { name: "Disable" }));
    await waitFor(() => expect(within(within(dialog).getByRole("row", { name: "Distribution EDEMO0001" })).getByText("Disabled")).toBeInTheDocument());
  });

  it("creates an invalidation from the bucket menu and shows it in the history timeline", async () => {
    const { user } = await renderApp();
    const row = await bucketRow("demo-website");
    fireEvent.contextMenu(row);
    await user.click(await screen.findByRole("menuitem", { name: "CloudFront" }));
    await user.click(within(screen.getByRole("menu", { name: "CloudFront" })).getByRole("menuitem", { name: "Create invalidation…" }));
    const dialog = await screen.findByRole("dialog", { name: "CloudFront manager" });
    expect(within(dialog).getByText("Distributions that serve s3://demo-website")).toBeInTheDocument();
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "Distribution" })).toHaveValue("EDEMO0001 Demo website"));
    const paths = within(dialog).getByRole("textbox", { name: "Paths" });
    await user.clear(paths);
    await user.type(paths, "/a*b");
    expect(await within(dialog).findByText(/only allowed as the last character/)).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Create invalidation" })).toBeDisabled();
    await user.clear(paths);
    await user.type(paths, "/images/*");
    await user.click(within(dialog).getByRole("button", { name: "Create invalidation" }));
    expect(await within(dialog).findByText("Invalidation created")).toBeInTheDocument();

    await user.click(within(dialog).getByRole("tab", { name: "History" }));
    const table = await within(dialog).findByRole("table", { name: "Invalidation history" });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(within(rows[0]!).getByText("InProgress")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("Completed")).toBeInTheDocument();
    await user.click(within(rows[0]!).getByRole("button", { name: /Show paths of/ }));
    expect(await within(table).findByText("/images/*")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Load more" }));
    await within(table).findByRole("row", { name: "Invalidation I0000" });
  });

  it("saves a path set and loads it again", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await openManager(user);
    const dialog = await screen.findByRole("dialog", { name: "CloudFront manager" });
    await within(dialog).findByRole("row", { name: "Distribution EDEMO0001" });
    await user.click(within(dialog).getByRole("tab", { name: "Invalidate" }));
    const paths = within(dialog).getByRole("textbox", { name: "Paths" });
    await user.clear(paths);
    await user.type(paths, "/css/*");
    await user.type(within(dialog).getByRole("textbox", { name: "Set name" }), "Styles");
    await user.click(within(dialog).getByRole("button", { name: "Save set" }));
    await user.clear(paths);
    await user.click(within(dialog).getByRole("combobox", { name: "Saved path sets" }));
    await user.click(await screen.findByRole("option", { name: "Styles" }));
    expect(paths).toHaveValue("/css/*");
  });

  it("attaches a distribution to a bucket from the bucket menu and shows the bucket policy", async () => {
    const { user } = await renderApp();
    const row = await bucketRow("demo-photos");
    fireEvent.contextMenu(row);
    await user.click(await screen.findByRole("menuitem", { name: "CloudFront" }));
    await user.click(within(screen.getByRole("menu", { name: "CloudFront" })).getByRole("menuitem", { name: "Attach distribution…" }));
    const form = await screen.findByRole("dialog", { name: "Attach distribution" });
    const comment = within(form).getByRole("textbox", { name: "Comment" });
    await user.clear(comment);
    await user.type(comment, "x".repeat(129));
    expect(within(form).getByText(/limited to 128 characters/)).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Create distribution" })).toBeDisabled();
    await user.clear(comment);
    await user.type(comment, "Photos CDN");
    await user.click(within(form).getByRole("button", { name: "Create distribution" }));
    expect(await within(form).findByText("Distribution created")).toBeInTheDocument();
    expect(within(form).getByRole<HTMLTextAreaElement>("textbox", { name: "Bucket policy" }).value).toContain("arn:aws:s3:::demo-photos/*");
    await user.click(within(form).getByRole("button", { name: "Close" }));
    const dialog = await screen.findByRole("dialog", { name: "CloudFront manager" });
    await within(dialog).findByRole("row", { name: "Distribution ENEW00004" });
  });

  it("edits a distribution from the bucket menu", async () => {
    const { user } = await renderApp();
    const row = await bucketRow("demo-website");
    fireEvent.contextMenu(row);
    await user.click(await screen.findByRole("menuitem", { name: "CloudFront" }));
    await user.click(within(screen.getByRole("menu", { name: "CloudFront" })).getByRole("menuitem", { name: "Edit distribution…" }));
    const form = await screen.findByRole("dialog", { name: "Edit distribution EDEMO0001" });
    const comment = within(form).getByRole("textbox", { name: "Comment" });
    await waitFor(() => expect(comment).toHaveValue("Demo website"));
    await user.clear(comment);
    await user.type(comment, "Marketing site");
    await user.type(within(form).getByRole("textbox", { name: "Aliases" }), "www.example.org");
    await user.click(within(form).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Edit distribution EDEMO0001" })).toBeNull());
    const dialog = await screen.findByRole("dialog", { name: "CloudFront manager" });
    await within(dialog).findByText("Marketing site");
  });

  it("detaches a bucket after typing its name", async () => {
    const { user } = await renderApp();
    const row = await bucketRow("demo-photos");
    fireEvent.contextMenu(row);
    await user.click(await screen.findByRole("menuitem", { name: "CloudFront" }));
    await user.click(within(screen.getByRole("menu", { name: "CloudFront" })).getByRole("menuitem", { name: "Detach distribution" }));
    const confirm = await screen.findByRole("dialog", { name: "Detach distribution" });
    expect(within(confirm).getByRole("button", { name: "Detach" })).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "demo-photos");
    await user.click(within(confirm).getByRole("button", { name: "Detach" }));
    const dialog = await screen.findByRole("dialog", { name: "CloudFront manager" });
    await waitFor(() => expect(within(dialog).queryByRole("row", { name: "Distribution EDEMO0003" })).toBeNull());
  });
});
