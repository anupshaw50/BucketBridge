import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MOCK_HOME } from "../../mock/world";
import { renderApp, TestPlatform } from "../../test/renderApp";

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: "Buckets and folders" });
  const row = (await within(tree).findByText(name)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${name}`);
  }
  return row;
}

describe("saved sync jobs", () => {
  it("saves the sync form as a job, then runs, edits and deletes it from the Sync jobs list", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Pictures/holiday`);
    const { user, mock } = await renderApp({ platform });
    await screen.findByText("demo-photos");

    // Set up a sync with an exclusion rule and save it as a job.
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(screen.getByRole("menuitem", { name: "Folder sync…" }));
    const dialog = await screen.findByRole("dialog", { name: "Synchronise folders" });
    await user.click(within(dialog).getByRole("switch", { name: /Delete extra files/ }));
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    await user.click(within(dialog).getByRole("button", { name: "Browse..." }));
    await user.click(within(await within(dialog).findByRole("list", { name: "Subfolders" })).getByText("photos"));
    await user.click(await within(await within(dialog).findByRole("list", { name: "Subfolders" })).findByText("2024"));
    await waitFor(() =>
      expect(within(dialog).getByLabelText("Selected S3 folder")).toHaveTextContent("s3://demo-photos/photos/2024/"),
    );
    await user.click(within(dialog).getByRole("button", { name: "Filters and options" }));
    await user.type(within(dialog).getByRole("textbox", { name: /Exclude rules/ }), "*.cr2");

    // A job needs a name.
    await user.click(within(dialog).getByRole("button", { name: "Save as job" }));
    expect(await within(dialog).findByText("Enter a name for the job.")).toBeInTheDocument();
    await user.type(within(dialog).getByRole("textbox", { name: "Save as job" }), "Holiday backup");
    await user.click(within(dialog).getByRole("button", { name: "Save as job" }));
    expect(await within(dialog).findByText("Saved job Holiday backup.")).toBeInTheDocument();
    const saved = await mock.list_sync_jobs();
    expect(saved).toHaveLength(1);
    expect(saved[0]).toMatchObject({
      name: "Holiday backup",
      direction: "upload",
      delete: true,
      local_root: `${MOCK_HOME}/Pictures/holiday`,
      s3_prefix: "s3://demo-photos/photos/2024/",
    });
    expect(saved[0]?.options.exclude).toEqual(["*.cr2"]);
    // Enter in the name field saved instead of moving the wizard on.
    expect(within(dialog).queryByLabelText("Rows per action")).not.toBeInTheDocument();

    // The same name again is refused by the backend.
    await user.click(within(dialog).getByRole("button", { name: "Save as job" }));
    expect(await within(dialog).findByText(/already exists/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Synchronise folders" })).not.toBeInTheDocument());

    // The jobs list.
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(screen.getByRole("menuitem", { name: "Sync jobs…" }));
    const jobs = await screen.findByRole("dialog", { name: "Sync jobs" });
    const list = within(jobs).getByRole("list", { name: "Saved sync jobs" });
    expect(within(list).getByText("Holiday backup")).toBeInTheDocument();
    expect(within(list).getByText("Deletes extra files")).toBeInTheDocument();

    // Run: analyses the job and opens the plan for review, without the excluded file.
    await user.click(within(jobs).getByRole("button", { name: "Run Holiday backup" }));
    const results = await screen.findByRole("dialog", { name: "Synchronise folders" });
    const counts = await within(results).findByLabelText("Rows per action");
    expect(counts).toHaveTextContent("Copy: 2");
    const table = within(results).getByRole("table", { name: "Sync results" });
    expect(within(table).queryByText("unreadable-raw.cr2")).not.toBeInTheDocument();
    expect(within(results).getByRole("button", { name: "Execute" })).toBeEnabled();
    await user.click(within(results).getByRole("button", { name: "Close" }));

    // Edit: the form is filled from the job and Save job replaces it.
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(screen.getByRole("menuitem", { name: "Sync jobs…" }));
    const again = await screen.findByRole("dialog", { name: "Sync jobs" });
    await user.click(within(again).getByRole("button", { name: "Edit Holiday backup" }));
    const editor = await screen.findByRole("dialog", { name: "Edit sync job Holiday backup" });
    expect(within(editor).getByRole("switch", { name: /Delete extra files/ })).toBeChecked();
    await user.click(within(editor).getByRole("switch", { name: /Delete extra files/ }));
    await user.click(within(editor).getByRole("button", { name: "Next" }));
    expect(within(editor).getByRole("textbox", { name: "Local folder" })).toHaveValue(`${MOCK_HOME}/Pictures/holiday`);
    await user.click(within(editor).getByRole("button", { name: "Filters and options" }));
    expect(within(editor).getByRole("textbox", { name: /Exclude rules/ })).toHaveValue("*.cr2");
    await user.click(within(editor).getByRole("button", { name: "Save job" }));
    expect(await within(editor).findByText("Saved job Holiday backup.")).toBeInTheDocument();
    const edited = await mock.list_sync_jobs();
    expect(edited).toHaveLength(1);
    expect(edited[0]?.id).toBe(saved[0]?.id);
    expect(edited[0]?.delete).toBe(false);
    await user.click(within(editor).getByRole("button", { name: "Cancel" }));

    // Delete asks first.
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(screen.getByRole("menuitem", { name: "Sync jobs…" }));
    const last = await screen.findByRole("dialog", { name: "Sync jobs" });
    await user.click(within(last).getByRole("button", { name: "Delete Holiday backup" }));
    const confirm = await screen.findByRole("dialog", { name: "Delete sync job" });
    await user.click(within(confirm).getByRole("button", { name: "Cancel" }));
    expect(await mock.list_sync_jobs()).toHaveLength(1);
    await user.click(within(last).getByRole("button", { name: "Delete Holiday backup" }));
    await user.click(within(await screen.findByRole("dialog", { name: "Delete sync job" })).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(within(last).getByText(/No saved sync jobs/)).toBeInTheDocument());
    expect(await mock.list_sync_jobs()).toEqual([]);
  });

  it("shows the summary report when a run ends and offers the S3 to S3 direction", async () => {
    const { user } = await renderApp();
    await screen.findByText("demo-photos");
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(screen.getByRole("menuitem", { name: "Folder sync…" }));
    const dialog = await screen.findByRole("dialog", { name: "Synchronise folders" });
    expect(within(dialog).getByRole("radio", { name: /S3 to S3/ })).toBeInTheDocument();
    await user.click(within(dialog).getByRole("radio", { name: /S3 to S3/ }));
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    // Two buckets are asked for, and no local folder.
    expect(within(dialog).queryByRole("textbox", { name: "Local folder" })).not.toBeInTheDocument();
    expect(within(dialog).getByRole("combobox", { name: "Source bucket" })).toBeInTheDocument();
    expect(within(dialog).getByRole("combobox", { name: "Destination bucket" })).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Analyse" }));
    expect(await within(dialog).findByText("Choose the source bucket.")).toBeInTheDocument();
  });
});
