import { afterEach, describe, expect, it } from "vitest";
import { BackendError } from "../api/errors";
import type { TransferTask } from "../api/types";
import { record, settle } from "../test/helpers";
import { MockBackend, type MockOptions } from "./mockBackend";
import { MOCK_HOME, MOCK_MFA_CODE } from "./world";

const backends: MockBackend[] = [];

function create(options: MockOptions = {}): MockBackend {
  const backend = new MockBackend({ autoRun: false, latencyMs: 0, logObjects: 30, ...options });
  backends.push(backend);
  return backend;
}

afterEach(() => {
  while (backends.length > 0) {
    backends.pop()?.dispose();
  }
});

async function accountId(backend: MockBackend, label: string): Promise<string> {
  const list = await backend.list_accounts();
  const found = list.accounts.find((account) => account.label === label);
  if (!found) {
    throw new Error(`no account ${label}`);
  }
  return found.id;
}

async function rejection(promise: Promise<unknown>): Promise<BackendError> {
  try {
    await promise;
  } catch (error) {
    if (error instanceof BackendError) {
      return error;
    }
    throw error;
  }
  throw new Error("expected the promise to reject");
}

describe("mock listing", () => {
  it("pages a delimiter listing with next_token in key order", async () => {
    const backend = create({ pageSize: 3 });
    const id = await accountId(backend, "Demo");
    const first = await backend.list_objects({ account_id: id, bucket: "demo-photos", prefix: "", flat: false, token: null });
    expect(first.entries.map((entry) => entry.key)).toEqual(["archive/", "documents/", "emoji \u{1F4F7}/"]);
    expect(first.entries.every((entry) => entry.is_prefix)).toBe(true);
    expect(first.next_token).not.toBeNull();
    const second = await backend.list_objects({
      account_id: id,
      bucket: "demo-photos",
      prefix: "",
      flat: false,
      token: first.next_token,
    });
    expect(second.entries.map((entry) => entry.key)).toEqual(["odd/", "photos/", "readme.md"]);
    expect(second.entries[2]?.is_prefix).toBe(false);
    const third = await backend.list_objects({
      account_id: id,
      bucket: "demo-photos",
      prefix: "",
      flat: false,
      token: second.next_token,
    });
    expect(third.entries.map((entry) => entry.key)).toEqual(["space folder/"]);
    expect(third.next_token).toBeNull();
  });

  it("lists flat, never returns the prefix itself and keeps empty segments", async () => {
    const backend = create({ pageSize: 2 });
    const id = await accountId(backend, "Demo");
    const keys: string[] = [];
    let token: string | null = null;
    do {
      const page = await backend.list_objects({ account_id: id, bucket: "demo-photos", prefix: "photos/", flat: true, token });
      keys.push(...page.entries.map((entry) => entry.key));
      token = page.next_token;
    } while (token !== null);
    expect(keys).toEqual([
      "photos/2024/beach.jpg",
      "photos/2024/café.png",
      "photos/2024/sunset at sea.jpg",
      "photos/2025/",
      "photos/2025/winter/snow.jpg",
    ]);
    const marker = await backend.list_objects({
      account_id: id,
      bucket: "demo-photos",
      prefix: "photos/2025/",
      flat: false,
      token: null,
    });
    expect(marker.entries.map((entry) => entry.key)).toEqual(["photos/2025/winter/"]);
    const odd = await backend.list_objects({ account_id: id, bucket: "demo-photos", prefix: "odd/", flat: false, token: null });
    expect(odd.entries.map((entry) => entry.key)).toEqual(["odd//"]);
  });

  it("lists prefixes before objects per page, unquoted etags and no checksums", async () => {
    const backend = create({ pageSize: 4 });
    const id = await accountId(backend, "Demo");
    const page = await backend.list_objects({ account_id: id, bucket: "demo-photos", prefix: "documents/", flat: false, token: null });
    expect(page.entries.every((entry) => entry.etag !== null && !entry.etag.includes('"'))).toBe(true);
    expect(page.entries.every((entry) => Object.values(entry.checksums).every((value) => value === null))).toBe(true);
    const mixed = await backend.list_objects({ account_id: id, bucket: "demo-photos", prefix: "", flat: false, token: null });
    const second = await backend.list_objects({
      account_id: id,
      bucket: "demo-photos",
      prefix: "",
      flat: false,
      token: mixed.next_token,
    });
    expect(second.entries.map((entry) => entry.key)).toEqual(["photos/", "space folder/", "readme.md"]);
    const flat = await backend.list_objects({ account_id: id, bucket: "demo-photos", prefix: "photos/", flat: true, token: null });
    const marker = flat.entries.find((entry) => entry.key === "photos/2025/");
    expect(marker).toMatchObject({ is_prefix: false, size: 0 });
  });

  it("rejects malformed tokens and prefixes", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const args = { account_id: id, bucket: "demo-photos", prefix: "", flat: false };
    expect((await rejection(backend.list_objects({ ...args, token: "garbage" }))).code).toBe("usage");
    expect((await rejection(backend.list_objects({ ...args, prefix: "photos", token: null }))).code).toBe("usage");
    expect((await rejection(backend.list_objects({ ...args, bucket: "nope-bucket", token: null }))).code).toBe("not_found");
  });
});

describe("mock security and accounts", () => {
  it("rejects a wrong secret with an auth error", async () => {
    const backend = create();
    const id = await accountId(backend, "Wrong secret");
    expect((await rejection(backend.list_buckets({ account_id: id }))).code).toBe("auth");
  });

  it("locks secrets behind the master password", async () => {
    const backend = create({ masterPassword: "correct horse", locked: true });
    const changes = record(backend, "security://changed");
    const id = await accountId(backend, "Demo");
    expect(await backend.security_status()).toEqual({ master_password_set: true, unlocked: false });
    expect((await rejection(backend.list_buckets({ account_id: id }))).code).toBe("locked");
    expect((await rejection(backend.unlock({ password: "wrong" }))).code).toBe("auth");
    expect(await backend.security_status()).toEqual({ master_password_set: true, unlocked: false });
    await backend.unlock({ password: "correct horse" });
    expect((await backend.list_buckets({ account_id: id })).length).toBeGreaterThan(0);
    await backend.lock();
    await settle();
    expect(changes).toEqual([
      { master_password_set: true, unlocked: true },
      { master_password_set: true, unlocked: false },
    ]);
  });

  it("changes and removes the master password only with the current one", async () => {
    const backend = create();
    await backend.set_master_password({ current: null, new_password: "first-password" });
    expect(await backend.security_status()).toEqual({ master_password_set: true, unlocked: true });
    expect((await rejection(backend.set_master_password({ current: "nope", new_password: null }))).code).toBe("auth");
    await backend.set_master_password({ current: "first-password", new_password: null });
    expect(await backend.security_status()).toEqual({ master_password_set: false, unlocked: true });
  });

  it("requires an MFA session for accounts with an MFA serial", async () => {
    const backend = create();
    const id = await accountId(backend, "Secure (MFA)");
    expect((await rejection(backend.list_buckets({ account_id: id }))).code).toBe("mfa_required");
    expect((await rejection(backend.mfa_login({ account_id: id, code: "000000" }))).code).toBe("auth");
    expect((await rejection(backend.mfa_login({ account_id: id, code: "12" }))).code).toBe("usage");
    await backend.mfa_login({ account_id: id, code: MOCK_MFA_CODE });
    expect((await backend.list_buckets({ account_id: id })).map((bucket) => bucket.name)).toEqual(["secure-vault"]);
  });

  it("enforces unique labels case-insensitively and moves the default on removal", async () => {
    const backend = create();
    const input = {
      label: "demo",
      access_key_id: "AKEY",
      secret_access_key: "s3cr3t-value-123",
      session_token: null,
      region: "us-east-1",
      endpoint: null,
      path_style: false,
      request_payer: false,
      mfa_serial: null,
    };
    expect((await rejection(backend.add_account({ input }))).code).toBe("conflict");
    const added = await backend.add_account({ input: { ...input, label: "Second" } });
    expect(JSON.stringify(added)).not.toContain("s3cr3t-value-123");
    const demoId = await accountId(backend, "Demo");
    await backend.remove_account({ id: demoId });
    const list = await backend.list_accounts();
    expect(list.accounts.some((account) => account.id === demoId)).toBe(false);
    expect(list.default_account_id).toBe(list.accounts[0]?.id);
  });

  it("reports bucket kinds, naming errors and non-empty buckets", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const kinds = Object.fromEntries((await backend.list_buckets({ account_id: id })).map((b) => [b.name, b.kind]));
    expect(kinds["demo-website"]).toBe("cloudfront");
    expect(kinds["demo-photos"]).toBe("aws");
    const external = await accountId(backend, "Archive (custom endpoint)");
    expect((await backend.list_buckets({ account_id: external })).every((b) => b.kind === "external")).toBe(true);
    expect((await rejection(backend.create_bucket({ account_id: id, name: "Bad_Name", region: null }))).code).toBe("usage");
    expect((await rejection(backend.create_bucket({ account_id: id, name: "demo-photos", region: null }))).code).toBe(
      "conflict",
    );
    expect((await rejection(backend.delete_bucket({ account_id: id, name: "demo-photos" }))).code).toBe("conflict");
    await backend.create_bucket({ account_id: id, name: "brand-new-bucket", region: "eu-west-1" });
    await backend.delete_bucket({ account_id: id, name: "brand-new-bucket" });
  });
});

describe("mock transfer queue", () => {
  async function demo(options: MockOptions = {}) {
    const backend = create(options);
    const id = await accountId(backend, "Demo");
    const updates = record(backend, "transfer://update");
    return { backend, id, updates };
  }

  it("runs an upload through queued, running, stopped and completed", async () => {
    const { backend, id } = await demo({ bytesPerSecond: 1024 * 1024 });
    const [task] = await backend.enqueue_upload({
      account_id: id,
      local_paths: [`${MOCK_HOME}/Documents/report.pdf`],
      bucket: "demo-backups",
      prefix: "docs/",
    });
    if (!task) throw new Error("no task");
    expect(task.state).toBe("queued");
    expect(task.dst).toEqual({ type: "s3", bucket: "demo-backups", key: "docs/report.pdf" });
    backend.advance(250);
    let current = (await backend.list_tasks()).find((item) => item.id === task.id);
    expect(current?.state).toBe("running");
    expect(current?.bytes_done).toBeGreaterThan(0);
    expect(current?.speed).toBeGreaterThan(0);
    expect(current?.eta).not.toBeNull();
    await backend.pause_task({ id: task.id });
    current = (await backend.list_tasks()).find((item) => item.id === task.id);
    expect(current?.state).toBe("stopped");
    const doneBefore = current?.bytes_done ?? 0;
    backend.advance(1000);
    expect((await backend.list_tasks()).find((item) => item.id === task.id)?.bytes_done).toBe(doneBefore);
    expect((await rejection(backend.retry_task({ id: task.id }))).code).toBe("conflict");
    await backend.resume_task({ id: task.id });
    backend.advance(5000);
    current = (await backend.list_tasks()).find((item) => item.id === task.id);
    expect(current?.state).toBe("completed");
    expect(backend.objectKeys("demo-backups")).toContain("docs/report.pdf");
  });

  it("respects max_concurrent_tasks", async () => {
    const { backend, id } = await demo({ manyFiles: 8 });
    const settings = await backend.get_settings();
    settings.transfer.max_concurrent_tasks = 3;
    await backend.save_settings({ settings });
    const tasks = await backend.enqueue_upload({
      account_id: id,
      local_paths: [`${MOCK_HOME}/Many files`],
      bucket: "demo-backups",
      prefix: "",
    });
    expect(tasks).toHaveLength(8);
    expect(new Set(tasks.map((task) => task.group_id)).size).toBe(1);
    backend.advance(1);
    const states = (await backend.list_tasks()).filter((task) => tasks.some((item) => item.id === task.id));
    expect(states.filter((task) => task.state === "running")).toHaveLength(3);
  });

  it("uploads folders with relative paths and empty-folder markers", async () => {
    const { backend, id } = await demo();
    const tasks = await backend.enqueue_upload({
      account_id: id,
      local_paths: [`${MOCK_HOME}/Pictures`],
      bucket: "demo-backups",
      prefix: "up/",
    });
    const keys = tasks.map((task) => (task.dst?.type === "s3" ? task.dst.key : "")).sort();
    expect(keys).toContain("up/Pictures/holiday/beach.jpg");
    expect(keys).toContain("up/Pictures/empty folder/");
  });

  it("strips the parent part of keys for downloads and copies", async () => {
    const { backend, id } = await demo();
    const downloads = await backend.enqueue_download({
      account_id: id,
      bucket: "demo-photos",
      keys: ["photos/2024/", "documents/notes.txt"],
      dest_dir: "/tmp/x",
    });
    const paths = downloads.map((task) => (task.dst?.type === "local" ? task.dst.path : ""));
    expect(paths).toContain("/tmp/x/2024/beach.jpg");
    expect(paths).toContain("/tmp/x/notes.txt");
    const copies = await backend.enqueue_copy({
      account_id: id,
      src_bucket: "demo-photos",
      keys: ["photos/2024/"],
      dst_bucket: "demo-backups",
      dst_prefix: "backup/",
    });
    expect(copies.map((task) => (task.dst?.type === "s3" ? task.dst.key : "")).sort()).toEqual([
      "backup/2024/beach.jpg",
      "backup/2024/café.png",
      "backup/2024/sunset at sea.jpg",
    ]);
    expect(
      (
        await rejection(
          backend.enqueue_move({
            account_id: id,
            src_bucket: "demo-photos",
            keys: ["photos/"],
            dst_bucket: "demo-photos",
            dst_prefix: "photos/2024/",
          }),
        )
      ).code,
    ).toBe("usage");
  });

  it("rejects sources that would land on the same destination and enqueues nothing", async () => {
    const { backend, id } = await demo();
    const before = (await backend.list_tasks()).length;
    const twins = await backend.enqueue_upload({
      account_id: id,
      local_paths: [`${MOCK_HOME}/Documents/budget 2026.xlsx`],
      bucket: "demo-photos",
      prefix: "archive/",
    });
    const nested = await backend.enqueue_upload({
      account_id: id,
      local_paths: [`${MOCK_HOME}/Documents/notes/todo.txt`],
      bucket: "demo-photos",
      prefix: "archive/2024/",
    });
    backend.advance(3000);
    expect(twins).toHaveLength(1);
    expect(nested).toHaveLength(1);
    const count = (await backend.list_tasks()).length;
    const move = await rejection(
      backend.enqueue_move({
        account_id: id,
        src_bucket: "demo-photos",
        keys: ["documents/budget 2026.xlsx", "archive/budget 2026.xlsx"],
        dst_bucket: "demo-photos",
        dst_prefix: "space folder/",
      }),
    );
    expect(move.code).toBe("conflict");
    expect(move.message).toContain("budget 2026.xlsx");
    const copy = await rejection(
      backend.enqueue_copy({
        account_id: id,
        src_bucket: "demo-photos",
        keys: ["photos/2024/", "archive/2024/"],
        dst_bucket: "demo-backups",
        dst_prefix: "",
      }),
    );
    expect(copy.code).toBe("conflict");
    const download = await rejection(
      backend.enqueue_download({
        account_id: id,
        bucket: "demo-photos",
        keys: ["documents/budget 2026.xlsx", "archive/budget 2026.xlsx"],
        dest_dir: "/tmp/x",
      }),
    );
    expect(download.code).toBe("conflict");
    const upload = await rejection(
      backend.enqueue_upload({
        account_id: id,
        local_paths: [`${MOCK_HOME}/Documents/budget 2026.xlsx`, `${MOCK_HOME}/Documents/budget 2026.xlsx`],
        bucket: "demo-backups",
        prefix: "",
      }),
    );
    expect(upload.code).toBe("conflict");
    // A failure part-way (the second key is missing) also enqueues nothing.
    const missing = await rejection(
      backend.enqueue_copy({
        account_id: id,
        src_bucket: "demo-photos",
        keys: ["readme.md", "does-not-exist.txt"],
        dst_bucket: "demo-backups",
        dst_prefix: "",
      }),
    );
    expect(missing.code).toBe("not_found");
    backend.advance(5000);
    expect((await backend.list_tasks()).length).toBe(count);
    expect(count).toBe(before + 2);
    expect(backend.objectKeys("demo-photos")).toEqual(
      expect.arrayContaining(["documents/budget 2026.xlsx", "archive/budget 2026.xlsx"]),
    );
    expect(backend.objectKeys("demo-backups")).not.toContain("readme.md");
  });

  it("fails a flaky upload once and succeeds on retry", async () => {
    const { backend, id } = await demo();
    const [task] = await backend.enqueue_upload({
      account_id: id,
      local_paths: [`${MOCK_HOME}/Pictures/holiday/flaky-video.mp4`],
      bucket: "demo-backups",
      prefix: "",
    });
    if (!task) throw new Error("no task");
    backend.advance(5000);
    let current = (await backend.list_tasks()).find((item) => item.id === task.id);
    expect(current?.state).toBe("failed");
    expect(current?.error).toMatch(/reset/);
    await backend.retry_task({ id: task.id });
    backend.advance(5000);
    current = (await backend.list_tasks()).find((item) => item.id === task.id);
    expect(current?.state).toBe("completed");
    expect(current?.attempts).toBe(2);
  });

  it("deletes a prefix as one task that counts objects", async () => {
    const { backend, id } = await demo();
    const tasks = await backend.enqueue_delete({ account_id: id, bucket: "demo-photos", keys: ["photos/"] });
    expect(tasks).toHaveLength(1);
    expect(tasks[0]?.bytes_total).toBe(5);
    backend.advance(1000);
    expect(backend.objectKeys("demo-photos").some((key) => key.startsWith("photos/"))).toBe(false);
  });

  it("renames with a conflict check and moves folders", async () => {
    const { backend, id } = await demo();
    const conflict = await rejection(
      backend.rename({ account_id: id, bucket: "demo-photos", from_key: "documents/notes.txt", to_key: "readme.md" }),
    );
    expect(conflict.code).toBe("conflict");
    const tasks = await backend.rename({
      account_id: id,
      bucket: "demo-photos",
      from_key: "documents/",
      to_key: "papers/",
    });
    expect(tasks.every((task) => task.kind === "move")).toBe(true);
    backend.advance(2000);
    const keys = backend.objectKeys("demo-photos");
    expect(keys).toContain("papers/notes.txt");
    expect(keys.some((key) => key.startsWith("documents/"))).toBe(false);
  });

  it("applies start, stop, cancel and clear rules", async () => {
    const { backend } = await demo();
    const removed = record(backend, "transfer://removed");
    const before = await backend.list_tasks();
    const byState = (tasks: TransferTask[], state: TransferTask["state"]) => tasks.filter((task) => task.state === state);
    expect(byState(before, "stopped")).toHaveLength(1);
    await backend.start_all();
    expect(byState(await backend.list_tasks(), "queued")).toHaveLength(1);
    await backend.stop_all();
    expect(byState(await backend.list_tasks(), "stopped")).toHaveLength(1);
    await backend.cancel_all();
    const afterCancel = await backend.list_tasks();
    expect(byState(afterCancel, "cancelled")).toHaveLength(2);
    expect(byState(afterCancel, "completed")).toHaveLength(1);
    await backend.clear_finished();
    await settle();
    expect(await backend.list_tasks()).toHaveLength(0);
    expect(removed[0]?.ids).toHaveLength(3);
    expect((await rejection(backend.pause_task({ id: "missing" }))).code).toBe("not_found");
  });

  it("rejects invalid transitions with conflict and keeps failed tasks on clear", async () => {
    const { backend, id } = await demo();
    const tasks = await backend.list_tasks();
    const completed = tasks.find((task) => task.state === "completed");
    const failed = tasks.find((task) => task.state === "failed");
    const stopped = tasks.find((task) => task.state === "stopped");
    if (!completed || !failed || !stopped) throw new Error("fixture changed");
    expect((await rejection(backend.pause_task({ id: completed.id }))).code).toBe("conflict");
    expect((await rejection(backend.cancel_task({ id: completed.id }))).code).toBe("conflict");
    expect((await rejection(backend.resume_task({ id: failed.id }))).code).toBe("conflict");
    expect((await rejection(backend.retry_task({ id: stopped.id }))).code).toBe("conflict");
    const [queued] = await backend.enqueue_upload({
      account_id: id,
      local_paths: [`${MOCK_HOME}/Documents/notes/todo.txt`],
      bucket: "demo-backups",
      prefix: "",
    });
    if (!queued) throw new Error("no task");
    expect((await rejection(backend.resume_task({ id: queued.id }))).code).toBe("conflict");
    await backend.cancel_task({ id: queued.id });
    await backend.cancel_task({ id: stopped.id });
    await backend.clear_finished();
    const left = await backend.list_tasks();
    expect(left.map((task) => task.state)).toEqual(["failed"]);
    await backend.cancel_task({ id: failed.id });
    expect((await backend.list_tasks())[0]?.state).toBe("cancelled");
  });

  it("emits a copy of the task on every change", async () => {
    const { backend, id, updates } = await demo();
    await backend.enqueue_upload({
      account_id: id,
      local_paths: [`${MOCK_HOME}/Documents/notes/todo.txt`],
      bucket: "demo-backups",
      prefix: "",
    });
    backend.advance(500);
    await settle();
    const states = updates.map((task) => task.state);
    expect(states[0]).toBe("queued");
    expect(states).toContain("running");
    expect(states[states.length - 1]).toBe("completed");
    expect(new Set(updates).size).toBe(updates.length);
  });
});

describe("mock sync", () => {
  it("plans all four row actions, executes, and resumes failed rows", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const progress = record(backend, "sync://progress");
    const rows = record(backend, "sync://row");
    const plan = await backend.sync_analyze({
      account_id: id,
      local_root: `${MOCK_HOME}/Pictures/holiday`,
      s3_prefix: "s3://demo-photos/photos/2024/",
      direction: "upload",
      delete: true,
    });
    const actions = Object.fromEntries(plan.rows.map((row) => [row.rel_path, `${row.action}:${row.target ?? "-"}`]));
    expect(actions).toEqual({
      "beach.jpg": "equal:-",
      "café.png": "delete:remote",
      "flaky-video.mp4": "copy:remote",
      "new-photo.jpg": "copy:remote",
      "sunset at sea.jpg": "delete:remote",
      "unreadable-raw.cr2": "failed:-",
    });
    expect(plan.rows.find((row) => row.action === "equal")?.done).toBe(true);
    expect(plan.rows.find((row) => row.action === "failed")?.error).toMatch(/Permission denied/);
    await backend.sync_execute({ plan_id: plan.id });
    backend.advance(10_000);
    await settle();
    const finished = progress.filter((item) => item.finished);
    expect(finished).toHaveLength(1);
    // Whole plan: 2 copy + 2 delete rows; failed = the failed row plus the flaky copy.
    expect(finished[0]).toMatchObject({ total: 4, done: 3, failed: 2 });
    expect(rows.map((event) => event.row.rel_path).sort()).toEqual([
      "café.png",
      "flaky-video.mp4",
      "new-photo.jpg",
      "sunset at sea.jpg",
    ]);
    const afterFirst = await backend.sync_get({ plan_id: plan.id });
    const flaky = afterFirst.rows.find((row) => row.rel_path === "flaky-video.mp4");
    expect(flaky).toMatchObject({ action: "copy", target: "remote", done: false });
    expect(flaky?.error).toMatch(/reset/);
    expect(afterFirst.rows.filter((row) => !row.done).map((row) => row.rel_path)).toEqual([
      "flaky-video.mp4",
      "unreadable-raw.cr2",
    ]);
    await backend.sync_execute({ plan_id: plan.id });
    backend.advance(10_000);
    await settle();
    expect(progress.filter((item) => item.finished)[1]).toMatchObject({ total: 4, done: 4, failed: 1 });
    expect(rows).toHaveLength(5);
    expect((await backend.sync_get({ plan_id: plan.id })).rows.find((row) => row.rel_path === "flaky-video.mp4")).toMatchObject({
      done: true,
      error: null,
    });
    expect(backend.objectKeys("demo-photos")).toContain("photos/2024/flaky-video.mp4");
    expect(backend.objectKeys("demo-photos")).not.toContain("photos/2024/café.png");
  });

  it("rejects deleting in both directions", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const error = await rejection(
      backend.sync_analyze({
        account_id: id,
        local_root: `${MOCK_HOME}/Pictures/holiday`,
        s3_prefix: "s3://demo-photos/photos/2024/",
        direction: "bidirectional",
        delete: true,
      }),
    );
    expect(error.code).toBe("usage");
  });

  it("detects both directions and skips rows outside the direction", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const download = await backend.sync_analyze({
      account_id: id,
      local_root: `${MOCK_HOME}/Pictures/holiday`,
      s3_prefix: "s3://demo-photos/photos/2024/",
      direction: "download",
      delete: false,
    });
    expect(download.rows.map((row) => `${row.rel_path}:${row.action}:${row.target ?? "-"}`)).toEqual([
      "beach.jpg:equal:-",
      "café.png:copy:local",
      "sunset at sea.jpg:copy:local",
      "unreadable-raw.cr2:failed:-",
    ]);
    const both = await backend.sync_analyze({
      account_id: id,
      local_root: `${MOCK_HOME}/Pictures/holiday`,
      s3_prefix: "s3://demo-photos/photos/2024/",
      direction: "bidirectional",
      delete: false,
    });
    expect(both.rows.filter((row) => row.action === "copy" && row.target === "remote")).toHaveLength(2);
    expect(both.rows.filter((row) => row.action === "copy" && row.target === "local")).toHaveLength(2);
  });

  it("cancels a running plan and leaves unprocessed rows for a resume", async () => {
    const backend = create({ bytesPerSecond: 1 });
    const id = await accountId(backend, "Demo");
    const plan = await backend.sync_analyze({
      account_id: id,
      local_root: `${MOCK_HOME}/Pictures/holiday`,
      s3_prefix: "s3://demo-photos/photos/2024/",
      direction: "upload",
      delete: true,
    });
    await backend.sync_execute({ plan_id: plan.id });
    backend.advance(250);
    await backend.sync_cancel({ plan_id: plan.id });
    const after = await backend.sync_get({ plan_id: plan.id });
    expect(after.rows.filter((row) => !row.done).length).toBeGreaterThan(1);
    expect((await rejection(backend.sync_get({ plan_id: "missing" }))).code).toBe("not_found");
  });
});

describe("mock settings, log and crash report", () => {
  it("validates settings ranges and keeps the proxy password reference", async () => {
    const backend = create();
    const settings = await backend.get_settings();
    settings.advanced.list_page_size = 0;
    settings.transfer.retry_attempts = 21;
    const invalid = await rejection(backend.save_settings({ settings }));
    expect(invalid.code).toBe("usage");
    expect(invalid.message).toContain("advanced.list_page_size");
    expect(invalid.message).toContain("transfer.retry_attempts");
    settings.advanced.list_page_size = 1000;
    settings.transfer.retry_attempts = 20;
    settings.transfer.bandwidth_limit_kibps = 104_857_600;
    await backend.save_settings({ settings });
    await backend.set_proxy_password({ password: "hunter2-proxy" });
    const withRef = await backend.get_settings();
    expect(withRef.proxy.password_ref).not.toBeNull();
    expect(JSON.stringify(withRef)).not.toContain("hunter2-proxy");
    withRef.proxy.password_ref = null;
    await backend.save_settings({ settings: withRef });
    expect((await backend.get_settings()).proxy.password_ref).not.toBeNull();
  });

  it("filters the log by level, text and limit and honours the minimum level", async () => {
    const backend = create();
    await backend.clear_log();
    backend.injectLog("error", "s3", "Upload failed: disk full");
    backend.injectLog("warning", "transfer", "Retrying part 3");
    backend.injectLog("information", "app", "Ready");
    expect((await backend.get_log({ levels: ["error"], text: null, limit: null })).map((e) => e.message)).toEqual([
      "Upload failed: disk full",
    ]);
    expect((await backend.get_log({ levels: null, text: "TRANSFER", limit: null })).map((e) => e.message)).toEqual([
      "Retrying part 3",
    ]);
    expect((await backend.get_log({ levels: null, text: null, limit: 1 })).map((e) => e.message)).toEqual(["Ready"]);
    const settings = await backend.get_settings();
    settings.logging.level = "error";
    await backend.save_settings({ settings });
    backend.injectLog("information", "app", "ignored");
    expect((await backend.get_log({ levels: null, text: "ignored", limit: null })).length).toBe(0);
    await backend.export_log({ path: "/home/demo/Documents/log.csv", format: "csv" });
    expect(backend.fs.isFile("/home/demo/Documents/log.csv")).toBe(true);
  });

  it("returns and dismisses the crash report", async () => {
    const backend = create({ crashReport: true });
    const report = await backend.last_crash_report();
    expect(report?.contents).toContain("panicked");
    expect(backend.fs.isFile(report?.path ?? "")).toBe(true);
    await backend.dismiss_crash_report({ path: report?.path ?? "" });
    expect(await backend.last_crash_report()).toBeNull();
  });

  it("creates presigned URLs within the allowed expiry", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const url = await backend.presign({
      account_id: id,
      bucket: "demo-photos",
      key: "photos/2024/sunset at sea.jpg",
      method: "get",
      expires_secs: 3600,
    });
    expect(url).toContain("photos/2024/sunset%20at%20sea.jpg");
    expect(url).toContain("X-Amz-Expires=3600");
    const tooLong = await rejection(
      backend.presign({ account_id: id, bucket: "demo-photos", key: "readme.md", method: "put", expires_secs: 604_801 }),
    );
    expect(tooLong.code).toBe("usage");
  });
});
