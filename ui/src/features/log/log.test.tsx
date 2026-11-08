import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { settle } from "../../test/helpers";
import { tr } from "../../i18n";
import { renderApp, statusBar } from "../../test/renderApp";

function messages(): string[] {
  const log = screen.getByRole("log", { name: tr("Log entries") });
  return Array.from(log.querySelectorAll(".bb-log-row")).map((row) => row.lastElementChild?.textContent ?? "");
}

describe("event log panel", () => {
  it("filters by level and text and tails live entries", async () => {
    const { user, mock } = await renderApp();
    await act(async () => {
      await mock.clear_log();
      mock.injectLog("error", "s3", "Upload of a.txt failed");
      mock.injectLog("warning", "transfer", "Retrying part 2");
      mock.injectLog("information", "app", "Ready to go");
    });
    await user.click(await screen.findByRole("button", { name: tr("Event log") }));
    const panel = await screen.findByRole("region", { name: tr("Event log") });
    await waitFor(() =>
      expect(messages()).toEqual(expect.arrayContaining(["Upload of a.txt failed", "Retrying part 2", "Ready to go"])),
    );

    await user.click(within(panel).getByRole("checkbox", { name: tr("Information") }));
    await user.click(within(panel).getByRole("checkbox", { name: tr("Warnings") }));
    await waitFor(() => expect(messages()).toEqual(["Upload of a.txt failed"]));

    await act(async () => {
      mock.injectLog("error", "sync", "Sync of b.txt failed");
      mock.injectLog("information", "app", "Not shown");
      await settle();
    });
    expect(messages()).toEqual(["Upload of a.txt failed", "Sync of b.txt failed"]);

    await user.type(within(panel).getByRole("textbox", { name: tr("Filter messages") }), "SYNC");
    await waitFor(() => expect(messages()).toEqual(["Sync of b.txt failed"]));
    await act(async () => {
      mock.injectLog("error", "s3", "Another failure");
      mock.injectLog("error", "sync", "sync again");
      await settle();
    });
    expect(messages()).toEqual(["Sync of b.txt failed", "sync again"]);
  });

  it("exports through a save dialog and clears after confirmation", async () => {
    const { user, mock, platform } = await renderApp();
    platform.saveAnswers.push("/home/demo/Documents/events.json", null);
    await user.click(await screen.findByRole("button", { name: tr("Event log") }));
    const panel = await screen.findByRole("region", { name: tr("Event log") });
    await user.click(within(panel).getByRole("button", { name: tr("Export") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Export as JSON...") }));
    await waitFor(() => expect(mock.fs.isFile("/home/demo/Documents/events.json")).toBe(true));
    expect(platform.saveRequests[0]).toMatchObject({ defaultPath: "bucketbridge-log.json" });
    expect(
      statusBar().getByText(tr("Event log exported to {path}.", { path: "/home/demo/Documents/events.json" })),
    ).toBeInTheDocument();
    await user.click(within(panel).getByRole("button", { name: tr("Export") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Export as CSV...") }));
    await waitFor(() => expect(platform.saveRequests).toHaveLength(2));

    await user.click(within(panel).getByRole("button", { name: tr("Clear") }));
    const confirm = await screen.findByRole("dialog", { name: tr("Clear the event log") });
    await user.click(within(confirm).getByRole("button", { name: tr("Clear log") }));
    expect(await within(panel).findByText(tr("No log entries"))).toBeInTheDocument();
    expect(await mock.get_log({ levels: null, text: null, limit: null })).toEqual([]);
  });
});
