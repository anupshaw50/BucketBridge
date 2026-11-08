import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { UserEvent } from "@testing-library/user-event";
import { tr } from "../../i18n";
import { renderApp } from "../../test/renderApp";
import { MAX_INVALIDATION_PATHS, pathsFromText, validatePaths } from "./paths";

/** Regex that matches the given text with anything in the place of a {placeholder}. */
function pattern(template: string, vars?: Record<string, string | number>): RegExp {
  const text = tr(template, vars).replace(/\{(\w+)\}/g, "§§§");
  return new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/§§§/g, ".*"));
}

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
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
  await user.click(await screen.findByRole("menuitem", { name: tr("CloudFront manager…") }));
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
    expect(result.ok ? "" : result.message).toContain(tr("a path must start with '/'"));
    expect(result.ok ? "" : result.message).toContain(tr("'*' is only allowed as the last character"));
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
    const dialog = await screen.findByRole("dialog", { name: tr("CloudFront manager") });
    await within(dialog).findByRole("row", { name: pattern("Distribution {id}", { id: "EDEMO0001" }) });
    expect(within(dialog).queryByRole("row", { name: pattern("Distribution {id}", { id: "EDEMO0003" }) })).toBeNull();
    await user.click(within(dialog).getByRole("button", { name: tr("Load more") }));
    await within(dialog).findByRole("row", { name: pattern("Distribution {id}", { id: "EDEMO0003" }) });
    const other = within(dialog).getByRole("row", { name: pattern("Distribution {id}", { id: "EDEMO0002" }) });
    expect(within(other).getByText(tr("Disabled"))).toBeInTheDocument();
    await user.click(within(other).getByRole("button", { name: tr("Enable {id}", { id: "EDEMO0002" }) }));
    const confirm = await screen.findByRole("dialog", { name: tr("Enable distribution") });
    await user.click(within(confirm).getByRole("button", { name: tr("Enable") }));
    await waitFor(() =>
      expect(
        within(within(dialog).getByRole("row", { name: pattern("Distribution {id}", { id: "EDEMO0002" }) })).getByText(tr("Enabled")),
      ).toBeInTheDocument(),
    );
  });

  it("asks for the id before disabling a distribution", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await openManager(user);
    const dialog = await screen.findByRole("dialog", { name: tr("CloudFront manager") });
    const row = await within(dialog).findByRole("row", { name: pattern("Distribution {id}", { id: "EDEMO0001" }) });
    await user.click(within(row).getByRole("button", { name: tr("Disable {id}", { id: "EDEMO0001" }) }));
    const confirm = await screen.findByRole("dialog", { name: tr("Disable distribution") });
    expect(within(confirm).getByRole("button", { name: tr("Disable") })).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "EDEMO0001");
    await user.click(within(confirm).getByRole("button", { name: tr("Disable") }));
    await waitFor(() =>
      expect(
        within(within(dialog).getByRole("row", { name: pattern("Distribution {id}", { id: "EDEMO0001" }) })).getByText(tr("Disabled")),
      ).toBeInTheDocument(),
    );
  });

  it("creates an invalidation from the bucket menu and shows it in the history timeline", async () => {
    const { user } = await renderApp();
    const row = await bucketRow("demo-website");
    fireEvent.contextMenu(row);
    await user.click(await screen.findByRole("menuitem", { name: tr("CloudFront") }));
    await user.click(within(screen.getByRole("menu", { name: tr("CloudFront") })).getByRole("menuitem", { name: tr("Create invalidation…") }));
    const dialog = await screen.findByRole("dialog", { name: tr("CloudFront manager") });
    expect(within(dialog).getByText(pattern("Distributions that serve s3://{bucket}", { bucket: "demo-website" }))).toBeInTheDocument();
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: tr("Distribution") })).toHaveValue("EDEMO0001 Demo website"));
    const paths = within(dialog).getByRole("textbox", { name: tr("Paths") });
    await user.clear(paths);
    await user.type(paths, "/a*b");
    expect(await within(dialog).findByText(pattern("'*' is only allowed as the last character"))).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: tr("Create invalidation") })).toBeDisabled();
    await user.clear(paths);
    await user.type(paths, "/images/*");
    await user.click(within(dialog).getByRole("button", { name: tr("Create invalidation") }));
    expect(await within(dialog).findByText(tr("Invalidation created"))).toBeInTheDocument();

    await user.click(within(dialog).getByRole("tab", { name: tr("History") }));
    const table = await within(dialog).findByRole("table", { name: tr("Invalidation history") });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(within(rows[0]!).getByText("InProgress")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("Completed")).toBeInTheDocument();
    await user.click(within(rows[0]!).getByRole("button", { name: pattern("Show paths of {id}") }));
    expect(await within(table).findByText("/images/*")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Load more") }));
    await within(table).findByRole("row", { name: pattern("Invalidation {id}", { id: "I0000" }) });
  });

  it("saves a path set and loads it again", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    await openManager(user);
    const dialog = await screen.findByRole("dialog", { name: tr("CloudFront manager") });
    await within(dialog).findByRole("row", { name: pattern("Distribution {id}", { id: "EDEMO0001" }) });
    await user.click(within(dialog).getByRole("tab", { name: tr("Invalidate") }));
    const paths = within(dialog).getByRole("textbox", { name: tr("Paths") });
    await user.clear(paths);
    await user.type(paths, "/css/*");
    await user.type(within(dialog).getByRole("textbox", { name: tr("Set name") }), "Styles");
    await user.click(within(dialog).getByRole("button", { name: tr("Save set") }));
    await user.clear(paths);
    await user.click(within(dialog).getByRole("combobox", { name: tr("Saved path sets") }));
    await user.click(await screen.findByRole("option", { name: "Styles" }));
    expect(paths).toHaveValue("/css/*");
  });

  it("attaches a distribution to a bucket from the bucket menu and shows the bucket policy", async () => {
    const { user } = await renderApp();
    const row = await bucketRow("demo-photos");
    fireEvent.contextMenu(row);
    await user.click(await screen.findByRole("menuitem", { name: tr("CloudFront") }));
    await user.click(within(screen.getByRole("menu", { name: tr("CloudFront") })).getByRole("menuitem", { name: tr("Attach distribution…") }));
    const form = await screen.findByRole("dialog", { name: tr("Attach distribution") });
    const comment = within(form).getByRole("textbox", { name: tr("Comment") });
    await user.clear(comment);
    await user.type(comment, "x".repeat(129));
    expect(within(form).getByText(pattern("The comment is limited to {n} characters.", { n: 128 }))).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: tr("Create distribution") })).toBeDisabled();
    await user.clear(comment);
    await user.type(comment, "Photos CDN");
    await user.click(within(form).getByRole("button", { name: tr("Create distribution") }));
    expect(await within(form).findByText(tr("Distribution created"))).toBeInTheDocument();
    expect(within(form).getByRole<HTMLTextAreaElement>("textbox", { name: tr("Bucket policy") }).value).toContain("arn:aws:s3:::demo-photos/*");
    await user.click(within(form).getByRole("button", { name: tr("Close") }));
    const dialog = await screen.findByRole("dialog", { name: tr("CloudFront manager") });
    await within(dialog).findByRole("row", { name: pattern("Distribution {id}", { id: "ENEW00004" }) });
  });

  it("edits a distribution from the bucket menu", async () => {
    const { user } = await renderApp();
    const row = await bucketRow("demo-website");
    fireEvent.contextMenu(row);
    await user.click(await screen.findByRole("menuitem", { name: tr("CloudFront") }));
    await user.click(within(screen.getByRole("menu", { name: tr("CloudFront") })).getByRole("menuitem", { name: tr("Edit distribution…") }));
    const form = await screen.findByRole("dialog", { name: pattern("Edit distribution {id}", { id: "EDEMO0001" }) });
    const comment = within(form).getByRole("textbox", { name: tr("Comment") });
    await waitFor(() => expect(comment).toHaveValue("Demo website"));
    await user.clear(comment);
    await user.type(comment, "Marketing site");
    await user.type(within(form).getByRole("textbox", { name: tr("Aliases") }), "www.example.org");
    await user.click(within(form).getByRole("button", { name: tr("Save") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: pattern("Edit distribution {id}", { id: "EDEMO0001" }) })).toBeNull());
    const dialog = await screen.findByRole("dialog", { name: tr("CloudFront manager") });
    await within(dialog).findByText("Marketing site");
  });

  it("detaches a bucket after typing its name", async () => {
    const { user } = await renderApp();
    const row = await bucketRow("demo-photos");
    fireEvent.contextMenu(row);
    await user.click(await screen.findByRole("menuitem", { name: tr("CloudFront") }));
    await user.click(within(screen.getByRole("menu", { name: tr("CloudFront") })).getByRole("menuitem", { name: tr("Detach distribution") }));
    const confirm = await screen.findByRole("dialog", { name: tr("Detach distribution") });
    expect(within(confirm).getByRole("button", { name: tr("Detach") })).toBeDisabled();
    await user.type(within(confirm).getByRole("textbox"), "demo-photos");
    await user.click(within(confirm).getByRole("button", { name: tr("Detach") }));
    const dialog = await screen.findByRole("dialog", { name: tr("CloudFront manager") });
    await waitFor(() => expect(within(dialog).queryByRole("row", { name: pattern("Distribution {id}", { id: "EDEMO0003" }) })).toBeNull());
  });
});
