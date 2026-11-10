import { screen, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { tr } from "../../i18n";
import { MockBackend } from "../../mock/mockBackend";
import { openBucket, renderApp } from "../../test/renderApp";

const MIB = 1024 * 1024;
const KEY = "db/2026-10-29.sql.gz";

async function openUploads(user: UserEvent): Promise<HTMLElement> {
  await openBucket(user, "demo-backups");
  await user.click(screen.getByRole("button", { name: tr("Tools") }));
  await user.click(await screen.findByRole("menuitem", { name: tr("Uncompleted uploads...") }));
  const dialog = await screen.findByRole("dialog", { name: tr("Uncompleted multipart uploads") });
  await within(dialog).findByRole("table", { name: tr("Uncompleted uploads") });
  return dialog;
}

async function pickUpload(user: UserEvent, dialog: HTMLElement): Promise<HTMLElement> {
  await user.click(within(dialog).getByRole("checkbox", { name: tr("Select {key} in {bucket}", { key: KEY, bucket: "demo-backups" }) }));
  await user.click(within(dialog).getByRole("button", { name: tr("Resume...") }));
  return screen.findByRole("dialog", { name: tr("Resume upload") });
}

function addFile(mock: MockBackend, path: string, size: number): void {
  mock.fs.writeFile(path, { size, modified: new Date().toISOString() });
}

describe("resume an unfinished upload", () => {
  it("needs exactly one selected upload", async () => {
    const { user } = await renderApp();
    const dialog = await openUploads(user);
    const resume = within(dialog).getByRole("button", { name: tr("Resume...") });
    expect(resume).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: tr("Select older") }));
    expect(resume).toBeDisabled();
  });

  it("verifies the chosen file, queues the missing parts and removes the upload from the list", async () => {
    const { user, mock, platform } = await renderApp();
    addFile(mock, "/home/demo/db.sql.gz", 20 * MIB + 123);
    platform.fileAnswers.push(["/home/demo/db.sql.gz"]);
    const dialog = await openUploads(user);
    const resume = await pickUpload(user, dialog);
    expect(within(resume).getByRole("button", { name: tr("Resume upload") })).toBeDisabled();

    await user.click(within(resume).getByRole("button", { name: tr("Choose file...") }));
    expect(await within(resume).findByText(tr("This file matches the upload"))).toBeInTheDocument();
    expect(within(resume).getByText(tr("{count} of {total} parts are already uploaded ({size}).", { count: 2, total: 3, size: "16.0 MiB" }))).toBeInTheDocument();
    expect(within(resume).getByText(tr("The uploaded parts were compared with the file."))).toBeInTheDocument();

    await user.click(within(resume).getByRole("button", { name: tr("Resume upload") }));
    await screen.findByRole("dialog", { name: tr("Uncompleted multipart uploads") });
    expect(screen.queryByRole("dialog", { name: tr("Resume upload") })).toBeNull();
    const tasks = await mock.list_tasks();
    expect(tasks.some((task) => task.kind === "upload" && task.dst?.type === "s3" && task.dst.key === KEY)).toBe(true);
    expect(within(dialog).queryByRole("checkbox", { name: tr("Select {key} in {bucket}", { key: KEY, bucket: "demo-backups" }) })).toBeNull();
  });

  it("refuses a file that does not match, with the reason, and queues nothing", async () => {
    const { user, mock, platform } = await renderApp();
    addFile(mock, "/home/demo/other.bin", 30 * MIB);
    addFile(mock, "/home/demo/tiny.bin", 3 * MIB);
    platform.fileAnswers.push(["/home/demo/tiny.bin"], ["/home/demo/other.bin"]);
    const before = (await mock.list_tasks()).length;
    const dialog = await openUploads(user);
    const resume = await pickUpload(user, dialog);
    const button = within(resume).getByRole("button", { name: tr("Resume upload") });

    await user.click(within(resume).getByRole("button", { name: tr("Choose file...") }));
    expect(await within(resume).findByText(tr("This file cannot finish the upload"))).toBeInTheDocument();
    expect(within(resume).getByText(tr("This file is smaller than the part of the upload that is already on the server, so it is not the file the upload was made from."))).toBeInTheDocument();
    expect(button).toBeDisabled();

    await user.click(within(resume).getByRole("button", { name: tr("Choose another file...") }));
    expect(await within(resume).findByText(tr("Part {part} on the server is not the same as this file, so it is not the file the upload was made from.", { part: 1 }))).toBeInTheDocument();
    expect(button).toBeDisabled();

    // The content check can be left out; the sizes still fit.
    await user.click(within(resume).getByRole("checkbox", { name: tr("Do not compare the content of the uploaded parts with the file") }));
    expect(await within(resume).findByText(tr("This file matches the upload"))).toBeInTheDocument();
    expect(within(resume).getByText(tr("The content of the uploaded parts was not compared; their sizes and positions fit."))).toBeInTheDocument();
    expect(await mock.list_tasks()).toHaveLength(before);
  });
});

describe("resume mock", () => {
  it("rejects a file that does not match when resuming", async () => {
    const mock = new MockBackend({ autoRun: false, latencyMs: 0 });
    const account = (await mock.list_accounts()).accounts[0];
    const upload = (await mock.list_multipart_uploads({ account_id: account?.id ?? "", buckets: ["demo-backups"] })).uploads[0];
    mock.fs.writeFile("/home/demo/x.bin", { size: 40 * MIB, modified: new Date().toISOString() });
    const args = {
      account_id: account?.id ?? "",
      bucket: "demo-backups",
      key: upload?.key ?? "",
      upload_id: upload?.upload_id ?? "",
      path: "/home/demo/x.bin",
      skip_part_verification: false,
    };
    await expect(mock.resume_multipart_upload(args)).rejects.toMatchObject({ code: "usage" });
    await expect(mock.check_multipart_resume({ ...args, path: "/home/demo/missing.bin" })).rejects.toMatchObject({ code: "not_found" });
    await expect(mock.check_multipart_resume({ ...args, upload_id: "nope" })).rejects.toMatchObject({ code: "not_found" });
    await expect(mock.check_multipart_resume({ ...args, path: "relative.bin" })).rejects.toMatchObject({ code: "usage" });
  });
});
