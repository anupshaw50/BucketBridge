import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { renderApp } from "../../test/renderApp";

async function bucketRow(name: string): Promise<HTMLElement> {
  const tree = await screen.findByRole("tree", { name: tr("Buckets and folders") });
  const row = (await within(tree).findByText(name)).closest<HTMLElement>('[role="treeitem"]');
  if (row === null) {
    throw new Error(`no tree row for ${name}`);
  }
  return row;
}

describe("mount as drive (T04)", () => {
  it("hides the menu entry when the build cannot mount", async () => {
    await renderApp();
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await screen.findByRole("menu");
    expect(screen.queryByRole("menuitem", { name: tr("Mount as drive") })).toBeNull();
  });

  it("mounts a bucket on a chosen folder and unmounts it", async () => {
    const { user, mock } = await renderApp({ mock: { mountSupported: true } });
    await waitFor(async () => expect((await mock.mount_status()).supported).toBe(true));
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(await screen.findByRole("menuitem", { name: tr("Mount as drive") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Mount {bucket} as drive", { bucket: "demo-photos" }) });
    const mountButton = within(dialog).getByRole("button", { name: tr("Mount") });
    await waitFor(() => expect(mountButton).toBeDisabled());

    await user.type(within(dialog).getByLabelText(tr("Mount folder")), "/mnt/photos");
    await waitFor(() => expect(mountButton).toBeEnabled());
    await user.click(mountButton);
    const table = await within(dialog).findByRole("table", { name: tr("Active mounts") });
    expect(within(table).getByText("/mnt/photos")).toBeInTheDocument();
    expect((await mock.mount_status()).mounts).toHaveLength(1);

    await user.click(within(table).getByRole("button", { name: tr("Unmount") }));
    await waitFor(() => expect(within(dialog).queryByRole("table", { name: tr("Active mounts") })).toBeNull());
    expect((await mock.mount_status()).mounts).toHaveLength(0);
  });

  it("rejects a relative mount point and a second mount on the same folder", async () => {
    const { mock } = await renderApp({ mock: { mountSupported: true } });
    const accountId = (await mock.list_accounts()).accounts[0]?.id ?? "";
    await expect(
      mock.mount_bucket({ account_id: accountId, bucket: "demo-photos", prefix: null, mount_point: "relative" }),
    ).rejects.toMatchObject({ code: "usage" });
    await mock.mount_bucket({ account_id: accountId, bucket: "demo-photos", prefix: null, mount_point: "/mnt/a" });
    await expect(
      mock.mount_bucket({ account_id: accountId, bucket: "demo-photos", prefix: null, mount_point: "/mnt/a" }),
    ).rejects.toMatchObject({ code: "conflict" });
    await expect(mock.unmount_bucket({ mount_point: "/mnt/none" })).rejects.toMatchObject({ code: "not_found" });
  });

  it("offers free drive letters on Windows and mounts on one", async () => {
    const { user, mock } = await renderApp({ mock: { mountSupported: true, mountPlatform: "windows" } });
    await waitFor(async () => expect((await mock.mount_status()).supported).toBe(true));
    expect((await mock.mount_status()).drive_letters).toEqual(["D", "E", "Z"]);
    fireEvent.contextMenu(await bucketRow("demo-photos"));
    await user.click(await screen.findByRole("menuitem", { name: tr("Mount as drive") }));
    const dialog = await screen.findByRole("dialog", { name: tr("Mount {bucket} as drive", { bucket: "demo-photos" }) });
    await user.type(within(dialog).getByLabelText(tr("Mount folder")), "z");
    await user.click(within(dialog).getByRole("button", { name: tr("Mount") }));
    const table = await within(dialog).findByRole("table", { name: tr("Active mounts") });
    expect(within(table).getByText("Z:")).toBeInTheDocument();
  });

  it("rejects a taken drive letter and a relative folder on Windows", async () => {
    const { mock } = await renderApp({ mock: { mountSupported: true, mountPlatform: "windows" } });
    const accountId = (await mock.list_accounts()).accounts[0]?.id ?? "";
    const mountOn = (mount_point: string) =>
      mock.mount_bucket({ account_id: accountId, bucket: "demo-photos", prefix: null, mount_point });
    await expect(mountOn("C:")).rejects.toMatchObject({ code: "usage" });
    await expect(mountOn("relative")).rejects.toMatchObject({ code: "usage" });
    await expect(mountOn("D:\\")).resolves.toMatchObject({ mount_point: "D:" });
    await expect(mountOn("d")).rejects.toMatchObject({ code: "conflict" });
    await expect(mock.unmount_bucket({ mount_point: "D:" })).resolves.toBeUndefined();
  });
});
