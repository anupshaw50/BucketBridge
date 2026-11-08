import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { CommandArgs } from "../../api/backend";
import { tr } from "../../i18n";
import { MOCK_HOME } from "../../mock/world";
import { settle } from "../../test/helpers";
import { findObjectRow, openBucket, renderApp, statusBar, TestPlatform } from "../../test/renderApp";

/** 将插值句子转成匹配正则；未提供的变量按 \S+ 匹配。 */
function rx(text: string): RegExp {
  const escaped = text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(escaped.replace(/\\\{\w+\\\}/g, "[\\s\\S]*?"));
}

const selectedNames = () =>
  within(screen.getByRole("grid", { name: tr("Objects") }))
    .queryAllByRole("row", { selected: true })
    .map((row) => row.querySelector(".bb-col-name span")?.textContent);

class CountingPlatform extends TestPlatform {
  folderPicks = 0;
  override pickFolder(): Promise<string | null> {
    this.folderPicks += 1;
    return super.pickFolder();
  }
}

describe("same-name collisions", () => {
  it("refuses to cut and paste two same-named files into one folder, keeping both", async () => {
    const { user, mock, advance } = await renderApp();
    const accountId = (await mock.list_accounts()).default_account_id ?? "";
    await act(async () => {
      await mock.enqueue_upload({
        account_id: accountId,
        local_paths: [`${MOCK_HOME}/Documents/budget 2026.xlsx`],
        bucket: "demo-photos",
        prefix: "archive/",
      });
      await settle();
    });
    await advance(3000);
    const named = () => mock.objectKeys("demo-photos").filter((key) => key.endsWith("budget 2026.xlsx"));
    expect(named()).toEqual(["archive/budget 2026.xlsx", "documents/budget 2026.xlsx"]);
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Flat browsing") }));
    await user.click(await findObjectRow("documents/budget 2026.xlsx"));
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("archive/budget 2026.xlsx"));
    await user.keyboard("{/Control}");
    await user.keyboard("{Control>}x{/Control}");
    await user.click(screen.getByRole("button", { name: tr("Flat browsing") }));
    await user.dblClick(await findObjectRow("space folder"));
    await findObjectRow("hello world.txt");
    expect(screen.getByRole("button", { name: tr("Paste") })).toBeDisabled();
    await user.keyboard("{Control>}v{/Control}");
    expect(await statusBar().findByText(rx(tr('Two or more selected items have the same name "{name}" and would {verb} to the same place. Select them separately.', { name: "budget 2026.xlsx" })))).toBeInTheDocument();
    await advance(5000);
    expect(named()).toEqual(["archive/budget 2026.xlsx", "documents/budget 2026.xlsx"]);
    expect((await mock.list_tasks()).filter((task) => task.kind === "move")).toEqual([]);
  });

  it("refuses to download two same-named files into one folder", async () => {
    const platform = new CountingPlatform();
    const { user, mock, advance } = await renderApp({ platform });
    const accountId = (await mock.list_accounts()).default_account_id ?? "";
    await act(async () => {
      for (const prefix of ["photos/2024/", "documents/"]) {
        await mock.enqueue_upload({
          account_id: accountId,
          local_paths: [`${MOCK_HOME}/Documents/notes/todo.txt`],
          bucket: "demo-photos",
          prefix,
        });
      }
      await settle();
    });
    await advance(3000);
    await openBucket(user, "demo-photos");
    await user.click(screen.getByRole("button", { name: tr("Flat browsing") }));
    await user.click(await findObjectRow("documents/todo.txt"));
    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("photos/2024/todo.txt"));
    await user.keyboard("{/Control}");
    const tasksBefore = (await mock.list_tasks()).length;
    await user.click(screen.getByRole("button", { name: tr("Download") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Download to...") }));
    expect(await statusBar().findByText(rx(tr('Two or more selected items have the same name "{name}" and would {verb} to the same place. Select them separately.', { name: "todo.txt" })))).toBeInTheDocument();
    expect(platform.folderPicks).toBe(0);
    expect((await mock.list_tasks()).length).toBe(tasksBefore);

    await user.keyboard("{Control>}");
    await user.click(await findObjectRow("photos/2024/todo.txt"));
    await user.click(await findObjectRow("readme.md"));
    await user.keyboard("{/Control}");
    platform.folderAnswers.push("/home/demo/Downloads");
    await user.click(screen.getByRole("button", { name: tr("Download") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Download to...") }));
    expect(await statusBar().findByText(rx(tr("{what}: {n} tasks added to the transfer queue.", { what: tr("Download"), n: "2" })))).toBeInTheDocument();
    expect((await mock.list_tasks()).length).toBe(tasksBefore + 2);
  });
});

describe("keyboard use of the object list", () => {
  it("keeps focus in the list across folder navigation", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("photos"));
    const grid = screen.getByRole("grid", { name: tr("Objects") });
    expect(grid).toHaveFocus();
    await user.keyboard("{Enter}");
    await findObjectRow("2024");
    expect(screen.getByRole("grid", { name: tr("Objects") })).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(selectedNames()).toEqual(["2024"]);
    await user.keyboard("{Enter}");
    await findObjectRow("beach.jpg");
    await user.keyboard("{Backspace}");
    await findObjectRow("2025");
    expect(screen.getByRole("grid", { name: tr("Objects") })).toHaveFocus();
    await user.keyboard("{Alt>}{ArrowUp}{/Alt}");
    await findObjectRow("readme.md");
    expect(screen.getByRole("grid", { name: tr("Objects") })).toHaveFocus();
  });

  it("goes up from an empty folder with the keyboard", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("archive"));
    await user.keyboard("{Enter}");
    expect(await screen.findByText(tr("This folder is empty"))).toBeInTheDocument();
    expect(screen.getByRole("grid", { name: tr("Objects") })).toHaveFocus();
    await user.keyboard("{Backspace}");
    await findObjectRow("readme.md");
  });

  it("selects the first row with Home and the last with End when no row has focus", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await findObjectRow("readme.md");
    screen.getByRole("grid", { name: tr("Objects") }).focus();
    await user.keyboard("{Home}");
    expect(selectedNames()).toEqual(["archive"]);
    await user.keyboard("{Escape}");
    await user.click(within(screen.getByRole("navigation", { name: tr("Location") })).getByRole("button", { name: "demo-photos" }));
    await findObjectRow("readme.md");
    screen.getByRole("grid", { name: tr("Objects") }).focus();
    await user.keyboard("{End}");
    expect(selectedNames()).toEqual(["readme.md"]);
  });

  it("sorts from the keyboard", async () => {
    const { user } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.dblClick(await findObjectRow("documents"));
    await findObjectRow("notes.txt");
    const sizeHeader = screen.getByRole("button", { name: tr("Sort by size") });
    expect(sizeHeader).not.toHaveAttribute("tabindex", "-1");
    sizeHeader.focus();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("columnheader", { name: new RegExp(tr("Size")) })).toHaveAttribute("aria-sort", "ascending");
    await user.keyboard(" ");
    expect(screen.getByRole("columnheader", { name: new RegExp(tr("Size")) })).toHaveAttribute("aria-sort", "descending");
  });
});

describe("downloads", () => {
  it("downloads to the default folder without asking, and asks for Download to", async () => {
    const platform = new CountingPlatform();
    platform.folderAnswers.push("/home/demo/Downloads", null);
    const { user, mock } = await renderApp({ platform });
    await user.click(await screen.findByRole("button", { name: tr("Settings") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Settings") });
    await user.click(within(dialog).getByRole("radio", { name: tr("Always download to this folder") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Browse...") }));
    await user.click(within(dialog).getByRole("button", { name: tr("OK") }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: tr("Settings") })).not.toBeInTheDocument());
    expect(platform.folderPicks).toBe(1);
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("photos"));
    await user.click(screen.getByRole("button", { name: tr("Download") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Download") }));
    expect(await statusBar().findByText(rx(tr("{what}: {n} tasks added to the transfer queue.", { what: tr("Download"), n: "5" })))).toBeInTheDocument();
    expect(platform.folderPicks).toBe(1);
    const downloads = (await mock.list_tasks()).filter((task) => task.kind === "download" && task.state === "queued");
    expect(downloads.map((task) => (task.dst?.type === "local" ? task.dst.path : ""))).toContain(
      "/home/demo/Downloads/photos/2024/beach.jpg",
    );
    await user.click(screen.getByRole("button", { name: tr("Download") }));
    await user.click(await screen.findByRole("menuitem", { name: tr("Download to...") }));
    await waitFor(() => expect(platform.folderPicks).toBe(2));
    expect((await mock.list_tasks()).filter((task) => task.kind === "download" && task.state === "queued")).toHaveLength(
      downloads.length,
    );
  });
});

describe("share link", () => {
  it("ignores a result for a method the user already changed and captions links by their own method", async () => {
    const { user, mock } = await renderApp();
    await openBucket(user, "demo-photos");
    await user.click(await findObjectRow("readme.md"));
    await user.click(screen.getByRole("button", { name: tr("Share link") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Share link") });
    const original = mock.presign.bind(mock);
    const calls: CommandArgs["presign"][] = [];
    const releases: (() => void)[] = [];
    mock.presign = (args) => {
      calls.push(args);
      return new Promise((resolve, reject) => {
        releases.push(() => {
          original(args).then(resolve, reject);
        });
      });
    };
    await user.click(within(dialog).getByRole("radio", { name: tr("Upload (PUT)") }));
    await user.click(within(dialog).getByRole("button", { name: tr("Create link") }));
    expect(within(dialog).getByRole("radio", { name: tr("Download (GET)") })).toBeDisabled();
    expect(within(dialog).getByRole("textbox", { name: tr("Valid for") })).toBeDisabled();
    await act(async () => {
      releases.shift()?.();
      await settle();
    });
    expect(within(dialog).getByText(rx(tr("Upload with an HTTP PUT request to this link until then.")))).toBeInTheDocument();
    expect(within(dialog).queryByText(rx(tr("Anyone with the link can download the object until then.")))).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("radio", { name: tr("Download (GET)") }));
    expect(within(dialog).queryByRole("textbox", { name: tr("Link") })).not.toBeInTheDocument();
    expect(calls.map((call) => call.method)).toEqual(["put"]);
  });
});
