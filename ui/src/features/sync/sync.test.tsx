import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MOCK_HOME } from "../../mock/world";
import { renderApp, TestPlatform } from "../../test/renderApp";

function resultPaths(): string[] {
  const table = screen.getByRole("table", { name: "Sync results" });
  return within(table)
    .getAllByRole("row")
    .slice(1)
    .map((row) => within(row).getAllByRole("cell")[0]?.textContent ?? "");
}

describe("sync wizard", () => {
  it("analyses, switches to failed rows, executes and resumes", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Pictures/holiday`);
    const { user, advance, mock } = await renderApp({ platform });
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: "Sync" }));
    const dialog = await screen.findByRole("dialog", { name: "Synchronise folders" });

    await user.click(within(dialog).getByRole("radio", { name: /Upload \(local to S3\)/ }));
    await user.click(within(dialog).getByRole("switch", { name: /Delete extra files/ }));
    await user.click(within(dialog).getByRole("button", { name: "Next" }));

    await user.click(within(dialog).getByRole("button", { name: "Analyse" }));
    expect(within(dialog).getByText("Choose the local folder.")).toBeInTheDocument();
    expect(within(dialog).getByText("Choose a bucket.")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Browse..." }));
    expect(within(dialog).getByRole("textbox", { name: "Local folder" })).toHaveValue(`${MOCK_HOME}/Pictures/holiday`);
    await user.click(within(dialog).getByRole("combobox", { name: "Bucket" }));
    await user.click(await within(dialog).findByRole("option", { name: "demo-photos" }));
    const folders = await within(dialog).findByRole("list", { name: "Subfolders" });
    await user.click(within(folders).getByText("photos"));
    await user.click(await within(await within(dialog).findByRole("list", { name: "Subfolders" })).findByText("2024"));
    await waitFor(() =>
      expect(within(dialog).getByLabelText("Selected S3 folder")).toHaveTextContent("s3://demo-photos/photos/2024/"),
    );
    await user.click(within(dialog).getByRole("button", { name: "Analyse" }));

    const counts = await within(dialog).findByLabelText("Rows per action");
    expect(counts).toHaveTextContent("Copy: 2");
    expect(counts).toHaveTextContent("Delete: 2");
    expect(counts).toHaveTextContent("Equal: 1");
    expect(counts).toHaveTextContent("Failed: 1");
    expect(resultPaths()).toEqual([
      "beach.jpg",
      "café.png",
      "flaky-video.mp4",
      "new-photo.jpg",
      "sunset at sea.jpg",
      "unreadable-raw.cr2",
    ]);
    await user.click(within(dialog).getByRole("radio", { name: /Failed only \(1\)/ }));
    expect(resultPaths()).toEqual(["unreadable-raw.cr2"]);

    await user.click(within(dialog).getByRole("button", { name: "Execute" }));
    expect(await within(dialog).findByRole("button", { name: "Cancel" })).toBeInTheDocument();
    await advance(10_000);
    expect(
      await within(dialog).findByText(
        "Finished: 3 of 4 applied, 2 failed. Resume retries 1 copy or delete row not done yet.",
      ),
    ).toBeInTheDocument();
    expect(within(dialog).getByRole("radio", { name: /Failed only \(2\)/ })).toBeInTheDocument();
    expect(resultPaths()).toEqual(["flaky-video.mp4", "unreadable-raw.cr2"]);
    const flakyRow = within(screen.getByRole("table", { name: "Sync results" })).getAllByRole("row")[1];
    if (!flakyRow) throw new Error("no row");
    expect(within(flakyRow).getByText("Copy")).toBeInTheDocument();
    expect(within(flakyRow).getByText(/Connection reset/)).toBeInTheDocument();
    expect(mock.objectKeys("demo-photos")).not.toContain("photos/2024/café.png");

    await user.click(within(dialog).getByRole("button", { name: "Resume" }));
    await advance(10_000);
    expect(await within(dialog).findByText("Finished: 4 of 4 applied, 1 failed.")).toBeInTheDocument();
    expect(resultPaths()).toEqual(["unreadable-raw.cr2"]);
    expect(within(dialog).getByRole("button", { name: "Resume" })).toBeDisabled();
    expect(mock.objectKeys("demo-photos")).toContain("photos/2024/flaky-video.mp4");

    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Synchronise folders" })).not.toBeInTheDocument());
    await user.click(screen.getByRole("button", { name: "Sync" }));
    const reopened = await screen.findByRole("dialog", { name: "Synchronise folders" });
    expect(within(reopened).getByLabelText("Rows per action")).toBeInTheDocument();
  });

  it("cancels a running execution", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Pictures/holiday`);
    const { user, advance } = await renderApp({ platform, mock: { bytesPerSecond: 1 } });
    await screen.findByText("demo-photos");
    await user.click(screen.getByText("demo-photos"));
    await user.click(screen.getByRole("button", { name: "Sync" }));
    const dialog = await screen.findByRole("dialog", { name: "Synchronise folders" });
    await user.click(within(dialog).getByRole("radio", { name: /Download \(S3 to local\)/ }));
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    await user.click(within(dialog).getByRole("button", { name: "Browse..." }));
    await user.click(within(await within(dialog).findByRole("list", { name: "Subfolders" })).getByText("photos"));
    await user.click(await within(await within(dialog).findByRole("list", { name: "Subfolders" })).findByText("2024"));
    await waitFor(() =>
      expect(within(dialog).getByLabelText("Selected S3 folder")).toHaveTextContent("s3://demo-photos/photos/2024/"),
    );
    await user.click(within(dialog).getByRole("button", { name: "Analyse" }));
    await within(dialog).findByLabelText("Rows per action");
    await user.click(within(dialog).getByRole("button", { name: "Execute" }));
    await advance(250);
    expect(within(dialog).getByRole("button", { name: "Back" })).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(await within(dialog).findByRole("button", { name: "Resume" })).toBeEnabled();
    expect(within(dialog).getByText(/Resume retries/)).toBeInTheDocument();
  });

  it("does not reopen another account's plan and names the account in the summary", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Pictures/holiday`);
    const { user } = await renderApp({ platform });
    await screen.findByText("demo-photos");
    await user.click(screen.getByText("demo-photos"));
    await user.click(screen.getByRole("button", { name: "Sync" }));
    let dialog = await screen.findByRole("dialog", { name: "Synchronise folders" });
    await user.click(within(dialog).getByRole("switch", { name: /Delete extra files/ }));
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    await user.click(within(dialog).getByRole("button", { name: "Browse..." }));
    await user.click(within(await within(dialog).findByRole("list", { name: "Subfolders" })).getByText("photos"));
    await user.click(await within(await within(dialog).findByRole("list", { name: "Subfolders" })).findByText("2024"));
    await waitFor(() => expect(within(dialog).getByLabelText("Selected S3 folder")).toHaveTextContent("photos/2024/"));
    await user.click(within(dialog).getByRole("button", { name: "Analyse" }));
    await within(dialog).findByLabelText("Rows per action");
    expect(within(dialog).getByText(/Account: Demo/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Synchronise folders" })).not.toBeInTheDocument());

    await user.click(screen.getByRole("button", { name: "Account: Demo" }));
    await user.click(await screen.findByRole("menuitem", { name: /Archive \(custom endpoint\)/ }));
    await screen.findByText("archive-2025");
    await user.click(screen.getByRole("button", { name: "Sync" }));
    dialog = await screen.findByRole("dialog", { name: "Synchronise folders" });
    expect(within(dialog).queryByLabelText("Rows per action")).not.toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: "Execute" })).not.toBeInTheDocument();
    expect(within(dialog).getByRole("radio", { name: /Upload \(local to S3\)/ })).toBeInTheDocument();
  });

  it("disables deleting for both directions and explains rows that cannot run", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Pictures/holiday`);
    const { user, mock } = await renderApp({ platform });
    await screen.findByText("demo-photos");
    await user.click(screen.getByText("demo-photos"));
    await user.click(screen.getByRole("button", { name: "Sync" }));
    const dialog = await screen.findByRole("dialog", { name: "Synchronise folders" });
    const deleteSwitch = within(dialog).getByRole("switch", { name: /Delete extra files/ });
    await user.click(deleteSwitch);
    expect(deleteSwitch).toBeChecked();
    await user.click(within(dialog).getByRole("radio", { name: /Both directions/ }));
    expect(deleteSwitch).toBeDisabled();
    expect(deleteSwitch).not.toBeChecked();
    expect(within(dialog).getByText(/Not available in both directions/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Next" }));
    await user.click(within(dialog).getByRole("button", { name: "Browse..." }));
    await user.click(within(await within(dialog).findByRole("list", { name: "Subfolders" })).getByText("photos"));
    await user.click(await within(await within(dialog).findByRole("list", { name: "Subfolders" })).findByText("2024"));
    await waitFor(() =>
      expect(within(dialog).getByLabelText("Selected S3 folder")).toHaveTextContent("s3://demo-photos/photos/2024/"),
    );
    await user.click(within(dialog).getByRole("button", { name: "Analyse" }));
    const counts = await within(dialog).findByLabelText("Rows per action");
    expect(counts).toHaveTextContent("Delete: 0");
    expect(mock.objectKeys("demo-photos")).toContain("photos/2024/café.png");
  });
});
