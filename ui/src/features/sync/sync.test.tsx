import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { plural } from "../../lib/format";
import { tr } from "../../i18n";
import { MOCK_HOME } from "../../mock/world";
import { renderApp, TestPlatform } from "../../test/renderApp";

function resultPaths(): string[] {
  const table = screen.getByRole("table", { name: tr("Sync results") });
  return within(table)
    .getAllByRole("row")
    .slice(1)
    .map((row) => within(row).getAllByRole("cell")[0]?.textContent ?? "");
}

/** An accessible-name matcher that accepts the label as a prefix (labels with a description are longer). */
function labelIn(label: string): (accessibleName: string) => boolean {
  return (accessibleName) => accessibleName.includes(label);
}

describe("sync wizard", () => {
  it("analyses, switches to failed rows, executes and resumes", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Pictures/holiday`);
    const { user, advance, mock } = await renderApp({ platform });
    await screen.findByText("demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Sync") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Synchronise folders") });

    await user.click(within(dialog).getByRole("radio", { name: tr("Upload (local to S3)") }));
    await user.click(within(dialog).getByRole("switch", { name: labelIn(tr("Delete extra files on the target")) }));
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));

    await user.click(within(dialog).getByRole("button", { name: tr("Analyse") }));
    expect(within(dialog).getByText(tr("Choose the local folder."))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("Choose a bucket."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Browse...") }));
    expect(within(dialog).getByRole("textbox", { name: tr("Local folder") })).toHaveValue(`${MOCK_HOME}/Pictures/holiday`);
    await user.click(within(dialog).getByRole("combobox", { name: tr("Bucket") }));
    await user.click(await within(dialog).findByRole("option", { name: "demo-photos" }));
    const folders = await within(dialog).findByRole("list", { name: tr("Subfolders") });
    await user.click(within(folders).getByText("photos"));
    await user.click(await within(await within(dialog).findByRole("list", { name: tr("Subfolders") })).findByText("2024"));
    await waitFor(() =>
      expect(within(dialog).getByLabelText(tr("Selected S3 folder"))).toHaveTextContent("s3://demo-photos/photos/2024/"),
    );
    await user.click(within(dialog).getByRole("button", { name: tr("Analyse") }));

    const counts = await within(dialog).findByLabelText(tr("Rows per action"));
    expect(counts).toHaveTextContent(tr("{action}: {n}", { action: tr("Copy"), n: "2" }));
    expect(counts).toHaveTextContent(tr("{action}: {n}", { action: tr("Delete"), n: "2" }));
    expect(counts).toHaveTextContent(tr("{action}: {n}", { action: tr("Equal"), n: "1" }));
    expect(counts).toHaveTextContent(tr("{action}: {n}", { action: tr("Failed"), n: "1" }));
    expect(resultPaths()).toEqual([
      "beach.jpg",
      "café.png",
      "flaky-video.mp4",
      "new-photo.jpg",
      "sunset at sea.jpg",
      "unreadable-raw.cr2",
    ]);
    await user.click(within(dialog).getByRole("radio", { name: tr("Failed only ({n})", { n: "1" }) }));
    expect(resultPaths()).toEqual(["unreadable-raw.cr2"]);

    await user.click(within(dialog).getByRole("button", { name: tr("Execute") }));
    expect(await within(dialog).findByRole("button", { name: tr("Cancel") })).toBeInTheDocument();
    await advance(10_000);
    expect(
      await within(dialog).findByText(
        `${tr("Finished: {done} of {total} applied, {failed} failed.", { done: "3", total: "4", failed: "2" })}${tr(
          " Resume retries {rows} not done yet.",
          { rows: plural(1, "copy or delete row") },
        )}`,
      ),
    ).toBeInTheDocument();
    expect(within(dialog).getByRole("radio", { name: tr("Failed only ({n})", { n: "2" }) })).toBeInTheDocument();
    expect(resultPaths()).toEqual(["flaky-video.mp4", "unreadable-raw.cr2"]);
    const flakyRow = within(screen.getByRole("table", { name: tr("Sync results") })).getAllByRole("row")[1];
    if (!flakyRow) throw new Error("no row");
    expect(within(flakyRow).getByText(tr("Copy"))).toBeInTheDocument();
    expect(within(flakyRow).getByText(/Connection reset/)).toBeInTheDocument();
    expect(mock.objectKeys("demo-photos")).not.toContain("photos/2024/café.png");

    await user.click(within(dialog).getByRole("button", { name: tr("Resume") }));
    await advance(10_000);
    expect(
      await within(dialog).findByText(
        tr("Finished: {done} of {total} applied, {failed} failed.", { done: "4", total: "4", failed: "1" }),
      ),
    ).toBeInTheDocument();
    expect(resultPaths()).toEqual(["unreadable-raw.cr2"]);
    expect(within(dialog).getByRole("button", { name: tr("Resume") })).toBeDisabled();
    expect(mock.objectKeys("demo-photos")).toContain("photos/2024/flaky-video.mp4");

    await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Synchronise folders") })).not.toBeInTheDocument());
    await user.click(screen.getByRole("button", { name: tr("Sync") }));
    const reopened = await screen.findByRole("dialog", { name: tr("Synchronise folders") });
    expect(within(reopened).getByLabelText(tr("Rows per action"))).toBeInTheDocument();
  });

  it("cancels a running execution", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Pictures/holiday`);
    const { user, advance } = await renderApp({ platform, mock: { bytesPerSecond: 1 } });
    await screen.findByText("demo-photos");
    await user.click(screen.getByText("demo-photos"));
    await user.click(screen.getByRole("button", { name: tr("Sync") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Synchronise folders") });
    await user.click(within(dialog).getByRole("radio", { name: tr("Download (S3 to local)") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Browse...") }));
    await user.click(within(await within(dialog).findByRole("list", { name: tr("Subfolders") })).getByText("photos"));
    await user.click(await within(await within(dialog).findByRole("list", { name: tr("Subfolders") })).findByText("2024"));
    await waitFor(() =>
      expect(within(dialog).getByLabelText(tr("Selected S3 folder"))).toHaveTextContent("s3://demo-photos/photos/2024/"),
    );
    await user.click(within(dialog).getByRole("button", { name: tr("Analyse") }));
    await within(dialog).findByLabelText(tr("Rows per action"));
    await user.click(within(dialog).getByRole("button", { name: tr("Execute") }));
    await advance(250);
    expect(within(dialog).getByRole("button", { name: tr("Back") })).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel") }));
    expect(await within(dialog).findByRole("button", { name: tr("Resume") })).toBeEnabled();
    expect(within(dialog).getByText(new RegExp(tr(" Resume retries")))).toBeInTheDocument();
  });

  it("does not reopen another account's plan and names the account in the summary", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Pictures/holiday`);
    const { user } = await renderApp({ platform });
    await screen.findByText("demo-photos");
    await user.click(screen.getByText("demo-photos"));
    await user.click(screen.getByRole("button", { name: tr("Sync") }));
    let dialog = await screen.findByRole("dialog", { name: tr("Synchronise folders") });
    await user.click(within(dialog).getByRole("switch", { name: labelIn(tr("Delete extra files on the target")) }));
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Browse...") }));
    await user.click(within(await within(dialog).findByRole("list", { name: tr("Subfolders") })).getByText("photos"));
    await user.click(await within(await within(dialog).findByRole("list", { name: tr("Subfolders") })).findByText("2024"));
    await waitFor(() => expect(within(dialog).getByLabelText(tr("Selected S3 folder"))).toHaveTextContent("photos/2024/"));
    await user.click(within(dialog).getByRole("button", { name: tr("Analyse") }));
    await within(dialog).findByLabelText(tr("Rows per action"));
    expect(within(dialog).getByText(new RegExp(tr("Account: {label}", { label: "Demo" })))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Close") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Synchronise folders") })).not.toBeInTheDocument());

    await user.click(screen.getByRole("button", { name: tr("Account: {label}", { label: "Demo" }) }));
    await user.click(await screen.findByRole("menuitem", { name: /Archive \(custom endpoint\)/ }));
    await screen.findByText("archive-2025");
    await user.click(screen.getByRole("button", { name: tr("Sync") }));
    dialog = await screen.findByRole("dialog", { name: tr("Synchronise folders") });
    expect(within(dialog).queryByLabelText(tr("Rows per action"))).not.toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: tr("Execute") })).not.toBeInTheDocument();
    expect(within(dialog).getByRole("radio", { name: tr("Upload (local to S3)") })).toBeInTheDocument();
  });

  it("disables deleting for both directions and explains rows that cannot run", async () => {
    const platform = new TestPlatform();
    platform.folderAnswers.push(`${MOCK_HOME}/Pictures/holiday`);
    const { user, mock } = await renderApp({ platform });
    await screen.findByText("demo-photos");
    await user.click(screen.getByText("demo-photos"));
    await user.click(screen.getByRole("button", { name: tr("Sync") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Synchronise folders") });
    const deleteSwitch = within(dialog).getByRole("switch", { name: labelIn(tr("Delete extra files on the target")) });
    await user.click(deleteSwitch);
    expect(deleteSwitch).toBeChecked();
    await user.click(within(dialog).getByRole("radio", { name: tr("Both directions") }));
    expect(deleteSwitch).toBeDisabled();
    expect(deleteSwitch).not.toBeChecked();
    expect(within(dialog).getByText(tr("Not available in both directions: a file found on one side only is copied to the other side."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Next") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Browse...") }));
    await user.click(within(await within(dialog).findByRole("list", { name: tr("Subfolders") })).getByText("photos"));
    await user.click(await within(await within(dialog).findByRole("list", { name: tr("Subfolders") })).findByText("2024"));
    await waitFor(() =>
      expect(within(dialog).getByLabelText(tr("Selected S3 folder"))).toHaveTextContent("s3://demo-photos/photos/2024/"),
    );
    await user.click(within(dialog).getByRole("button", { name: tr("Analyse") }));
    const counts = await within(dialog).findByLabelText(tr("Rows per action"));
    expect(counts).toHaveTextContent(tr("{action}: {n}", { action: tr("Delete"), n: "0" }));
    expect(mock.objectKeys("demo-photos")).toContain("photos/2024/café.png");
  });
});
