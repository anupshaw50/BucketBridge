import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { renderApp, statusBar } from "../../test/renderApp";

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

describe("bb:// links", () => {
  it("open the account's bucket and folder when the app runs", async () => {
    const { mock } = await renderApp();
    const [account] = (await mock.list_accounts()).accounts;
    if (account === undefined) {
      throw new Error("the mock has no account");
    }
    mock.receiveLink({ type: "open", account_id: account.id, bucket: "demo-photos", prefix: "" });
    await flush();
    expect(await screen.findByRole("grid", { name: tr("Objects") })).toBeInTheDocument();
    expect(mock.quitCalls).toEqual([]);
    // The link was taken: nothing is pending any more.
    expect(await mock.take_pending_link()).toBeNull();
  });

  it("show a refused link as an error and navigate nowhere", async () => {
    const { mock, notices } = await renderApp();
    mock.receiveLink({ type: "error", message: "the link names an unknown account: nope" });
    await flush();
    await waitFor(() => expect(notices).toHaveLength(1));
    expect(notices[0]?.title).toBe(tr("The link could not be opened"));
    expect(notices[0]?.text).toBe("the link names an unknown account: nope");
    await statusBar().findByText("the link names an unknown account: nope");
    expect(screen.queryByRole("grid", { name: tr("Objects") })).toBeNull();
  });

  it("that started the app are opened once the app is ready", async () => {
    const { mock } = await renderApp({
      // A link waits in the backend before the UI starts.
      prepare: async (backend) => {
        const [account] = (await backend.list_accounts()).accounts;
        if (account === undefined) {
          throw new Error("the mock has no account");
        }
        backend.receiveLink({ type: "open", account_id: account.id, bucket: "demo-photos", prefix: "" });
      },
    });
    expect(await screen.findByRole("grid", { name: tr("Objects") })).toBeInTheDocument();
    expect(await mock.take_pending_link()).toBeNull();
  });
});

describe("quit dialog", () => {
  it("asks what to do while tasks are active and can finish them first", async () => {
    const { mock, user } = await renderApp();
    mock.requestQuit({ active_transfers: 3, active_syncs: 1 });
    const dialog = await screen.findByRole("dialog", { name: tr("Quit BucketBridge?") });
    expect(within(dialog).getByText(tr("{n} transfers are running or waiting.", { n: 3 }))).toBeInTheDocument();
    expect(within(dialog).getByText(tr("1 sync run is in progress."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Finish running tasks, then quit") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Quit BucketBridge?") })).toBeNull());
    expect(mock.quitCalls).toEqual(["when_idle"]);
  });

  it("quits now on request", async () => {
    const { mock, user } = await renderApp();
    mock.requestQuit({ active_transfers: 1, active_syncs: 0 });
    const dialog = await screen.findByRole("dialog", { name: tr("Quit BucketBridge?") });
    expect(within(dialog).getByText(tr("1 transfer is running or waiting."))).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: tr("Quit now") }));
    await waitFor(() => expect(mock.quitCalls).toEqual(["now"]));
  });

  it("does nothing on cancel", async () => {
    const { mock, user } = await renderApp();
    mock.requestQuit({ active_transfers: 1, active_syncs: 0 });
    const dialog = await screen.findByRole("dialog", { name: tr("Quit BucketBridge?") });
    await user.click(within(dialog).getByRole("button", { name: tr("Cancel") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Quit BucketBridge?") })).toBeNull());
    expect(mock.quitCalls).toEqual([]);
  });
});

describe("everyday settings", () => {
  it("save keep-awake and the quit behaviour", async () => {
    const { mock, user } = await renderApp();
    expect((await mock.get_settings()).general.keep_awake).toBe(true);
    await user.click(screen.getByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("switch", { name: new RegExp(tr("Keep the computer awake during transfers")) }));
    await user.click(within(dialog).getByRole("radio", { name: tr("Let running tasks finish, then quit") }));
    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Settings") })).not.toBeInTheDocument());
    const saved = await mock.get_settings();
    expect(saved.general.keep_awake).toBe(false);
    expect(saved.general.quit_with_tasks).toBe("wait");
  });
});
