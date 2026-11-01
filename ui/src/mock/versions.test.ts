import { afterEach, describe, expect, it } from "vitest";
import { BackendError } from "../api/errors";
import type { VersionFilter } from "../api/types";
import { DEFAULT_FILTER } from "../features/versions/filters";
import { MockBackend } from "./mockBackend";
import { MOCK_HOME } from "./world";

const backends: MockBackend[] = [];

function create(options: ConstructorParameters<typeof MockBackend>[0] = {}): MockBackend {
  const backend = new MockBackend({ autoRun: false, latencyMs: 0, logObjects: 30, ...options });
  backends.push(backend);
  return backend;
}

afterEach(() => {
  while (backends.length > 0) {
    backends.pop()?.dispose();
  }
});

async function demoId(backend: MockBackend): Promise<string> {
  const list = await backend.list_accounts();
  const found = list.accounts.find((account) => account.label === "Demo");
  if (!found) {
    throw new Error("no demo account");
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

function filter(patch: Partial<VersionFilter>): VersionFilter {
  return { ...DEFAULT_FILTER, ...patch };
}

describe("mock versions", () => {
  it("lists a prefix grouped by key, newest first, with deleted keys", async () => {
    const backend = create();
    const account = await demoId(backend);
    const page = await backend.list_versions({
      account_id: account,
      bucket: "demo-photos",
      prefix: "documents/",
      exact: false,
      filter: DEFAULT_FILTER,
      token: null,
    });
    expect(page.groups.map((group) => group.key)).toEqual([
      "documents/budget 2026.xlsx",
      "documents/notes.txt",
      "documents/old-draft.txt",
      "documents/report-q3.pdf",
    ]);
    const notes = page.groups.find((group) => group.key === "documents/notes.txt");
    expect(notes?.versions.map((v) => v.version_id).slice(1)).toEqual(["v-notes-3", "v-notes-2", "v-notes-1"]);
    expect(notes?.versions.map((v) => v.is_latest)).toEqual([true, false, false, false]);
    expect(notes?.is_deleted).toBe(false);
    const draft = page.groups.find((group) => group.key === "documents/old-draft.txt");
    expect(draft?.is_deleted).toBe(true);
    expect(draft?.versions[0]).toMatchObject({ version_id: "dm-draft", is_delete_marker: true, is_latest: true, size: 0 });
    expect(draft?.total).toBe(3);
  });

  it("applies the filters like the core does", async () => {
    const backend = create();
    const account = await demoId(backend);
    const keys = async (patch: Partial<VersionFilter>) =>
      (
        await backend.list_versions({
          account_id: account,
          bucket: "demo-photos",
          prefix: "documents/",
          exact: false,
          filter: filter(patch),
          token: null,
        })
      ).groups.map((group) => `${group.key.replace("documents/", "")}:${group.versions.length}`);
    expect(await keys({ delete_markers: "only" })).toEqual(["old-draft.txt:1"]);
    expect(await keys({ delete_markers: "exclude", multiple_versions: true })).toEqual([
      "notes.txt:4",
      "old-draft.txt:2",
      "report-q3.pdf:2",
    ]);
    expect(await keys({ show_versions: false })).toEqual(["old-draft.txt:1"]);
    expect(await keys({ count_greater_than: 3 })).toEqual(["notes.txt:4"]);
    expect(await keys({ count_less_than: 2 })).toEqual(["budget 2026.xlsx:1"]);
    expect(await keys({ modified_after: "2026-10-10T00:00:00Z", modified_before: "2026-10-15T00:00:00Z" })).toEqual([
      "notes.txt:1",
      "old-draft.txt:1",
      "report-q3.pdf:1",
    ]);
    for (const bad of [
      filter({ show_versions: false, delete_markers: "exclude" }),
      filter({ modified_after: "2026-10-15T00:00:00Z", modified_before: "2026-10-15T00:00:00Z" }),
      filter({ count_greater_than: 4, count_less_than: 5 }),
    ]) {
      const error = await rejection(
        backend.list_versions({ account_id: account, bucket: "demo-photos", prefix: "", exact: false, filter: bad, token: null }),
      );
      expect(error.code).toBe("usage");
    }
  });

  it("pages by key with tokens and lists one key exactly", async () => {
    const backend = create({ pageSize: 2 });
    const account = await demoId(backend);
    const args = { account_id: account, bucket: "demo-photos", prefix: "documents/", exact: false, filter: DEFAULT_FILTER };
    const first = await backend.list_versions({ ...args, token: null });
    expect(first.groups).toHaveLength(2);
    expect(first.next_token).not.toBeNull();
    const second = await backend.list_versions({ ...args, token: first.next_token });
    expect(second.groups.map((g) => g.key)).toEqual(["documents/old-draft.txt", "documents/report-q3.pdf"]);
    expect(second.next_token).toBeNull();
    expect((await rejection(backend.list_versions({ ...args, token: "garbage" }))).code).toBe("usage");
    const exact = await backend.list_versions({ ...args, prefix: "readme.md", exact: true, token: null });
    expect(exact.groups.map((g) => g.key)).toEqual(["readme.md"]);
    expect((await rejection(backend.list_versions({ ...args, prefix: "documents/", exact: true, token: null }))).code).toBe("usage");
  });

  it("restores a version as a new current one, keeping every older version", async () => {
    const backend = create();
    const account = await demoId(backend);
    const args = { account_id: account, bucket: "demo-photos", key: "readme.md" };
    const outcome = await backend.restore_version({ ...args, version_id: "v-readme-2" });
    expect(outcome).toMatchObject({ key: "readme.md", restored_version_id: "v-readme-2", size: 900, etag: "etag-v-readme-2" });
    expect(outcome.version_id).not.toBeNull();
    const listed = await backend.list_versions({
      ...{ account_id: account, bucket: "demo-photos", prefix: "readme.md", exact: true },
      filter: DEFAULT_FILTER,
      token: null,
    });
    const versions = listed.groups[0]?.versions ?? [];
    expect(versions.map((v) => v.version_id)).toEqual([outcome.version_id, "ver-0001", "v-readme-2", "v-readme-1"]);
    expect(versions[0]).toMatchObject({ is_latest: true, size: 900 });
    // The key itself now has the restored content.
    const objects = await backend.list_objects({ account_id: account, bucket: "demo-photos", prefix: "", flat: false, token: null });
    expect(objects.entries.find((entry) => entry.key === "readme.md")?.size).toBe(900);

    expect((await rejection(backend.restore_version({ ...args, version_id: outcome.version_id ?? "" }))).code).toBe("conflict");
    expect((await rejection(backend.restore_version({ ...args, version_id: "nope" }))).code).toBe("not_found");
    expect((await rejection(backend.restore_version({ ...args, version_id: "" }))).code).toBe("usage");
    const marker = await rejection(backend.restore_version({ ...args, key: "documents/old-draft.txt", version_id: "dm-draft" }));
    expect(marker.code).toBe("usage");
    expect(marker.message).toMatch(/delete marker/);
  });

  it("brings a deleted key back by restoring a version or by removing the delete marker", async () => {
    const backend = create();
    const account = await demoId(backend);
    const draft = { account_id: account, bucket: "demo-photos" };
    const names = async () =>
      (await backend.list_objects({ ...draft, prefix: "documents/", flat: false, token: null })).entries.map((e) => e.key);
    expect(await names()).not.toContain("documents/old-draft.txt");
    const outcome = await backend.purge_versions({ ...draft, versions: [{ key: "documents/old-draft.txt", version_id: "dm-draft" }] });
    expect(outcome).toEqual({ deleted: 1, failures: [] });
    expect(await names()).toContain("documents/old-draft.txt");
    // The previous version is current again, with its own size.
    const objects = await backend.list_objects({ ...draft, prefix: "documents/", flat: false, token: null });
    expect(objects.entries.find((e) => e.key === "documents/old-draft.txt")?.size).toBe(5_120);

    await backend.restore_version({ ...draft, key: "documents/old-draft.txt", version_id: "v-draft-1" });
    const again = await backend.list_objects({ ...draft, prefix: "documents/", flat: false, token: null });
    expect(again.entries.find((e) => e.key === "documents/old-draft.txt")?.size).toBe(2_300);
  });

  it("purges versions for good, once each, and a purge of everything removes the key", async () => {
    const backend = create();
    const account = await demoId(backend);
    const base = { account_id: account, bucket: "demo-photos" };
    const outcome = await backend.purge_versions({
      ...base,
      versions: [
        { key: "readme.md", version_id: "v-readme-1" },
        { key: "readme.md", version_id: "v-readme-1" },
        { key: "readme.md", version_id: "never-existed" },
      ],
    });
    expect(outcome.deleted).toBe(2);
    const listed = await backend.list_versions({ ...base, prefix: "readme.md", exact: true, filter: DEFAULT_FILTER, token: null });
    expect(listed.groups[0]?.versions.map((v) => v.version_id)).toEqual(["ver-0001", "v-readme-2"]);
    expect((await rejection(backend.purge_versions({ ...base, versions: [{ key: "readme.md", version_id: "" }] }))).code).toBe("usage");
    // Purging the current version promotes the previous one.
    await backend.purge_versions({ ...base, versions: [{ key: "readme.md", version_id: "ver-0001" }] });
    const objects = await backend.list_objects({ ...base, prefix: "", flat: false, token: null });
    expect(objects.entries.find((e) => e.key === "readme.md")?.size).toBe(900);
    await backend.purge_versions({ ...base, versions: [{ key: "readme.md", version_id: "v-readme-2" }] });
    expect(
      (await backend.list_objects({ ...base, prefix: "", flat: false, token: null })).entries.some((e) => e.key === "readme.md"),
    ).toBe(false);
  });

  it("queues the download of one version as a transfer task that writes that version's size", async () => {
    const backend = create();
    const account = await demoId(backend);
    const base = { account_id: account, bucket: "demo-photos", key: "documents/old-draft.txt" };
    const dest = `${MOCK_HOME}/Downloads/old-draft (v2).txt`;
    const tasks = await backend.enqueue_download_version({ ...base, version_id: "v-draft-2", dest_path: dest });
    expect(tasks).toHaveLength(1);
    expect(tasks[0]).toMatchObject({
      kind: "download",
      src: { type: "s3", bucket: "demo-photos", key: "documents/old-draft.txt" },
      dst: { type: "local", path: dest },
      bytes_total: 5_120,
    });
    backend.advance(5_000);
    expect(backend.fs.stat(dest)?.size).toBe(5_120);
    expect((await backend.list_tasks()).find((t) => t.id === tasks[0]?.id)?.state).toBe("completed");

    expect((await rejection(backend.enqueue_download_version({ ...base, version_id: "dm-draft", dest_path: dest }))).code).toBe("usage");
    expect((await rejection(backend.enqueue_download_version({ ...base, version_id: "nope", dest_path: dest }))).code).toBe("not_found");
    expect((await rejection(backend.enqueue_download_version({ ...base, version_id: "v-draft-2", dest_path: "relative.txt" }))).code).toBe("usage");
    expect((await rejection(backend.enqueue_download_version({ ...base, key: "documents/", version_id: "v", dest_path: dest }))).code).toBe("usage");
  });
});

describe("mock previews", () => {
  async function preview(
    backend: MockBackend,
    bucket: string,
    key: string,
    page = 0,
    pageSize: number | null = null,
    versionId: string | null = null,
  ) {
    return backend.preview_object({
      account_id: await demoId(backend),
      bucket,
      key,
      version_id: versionId,
      page,
      page_size: pageSize,
    });
  }

  it("shows text in pages that tile the object, and the kind of everything else", async () => {
    const backend = create();
    const first = await preview(backend, "demo-logs", "audit-export.csv");
    expect(first).toMatchObject({ kind: "text", size: 614_400, page: 0, page_count: 5, offset: 0, length: 131_072, page_size: 131_072 });
    expect(first.text?.length).toBe(131_072);
    expect(first.text?.startsWith("audit-export.csv:")).toBe(true);
    const last = await preview(backend, "demo-logs", "audit-export.csv", 4);
    expect(last).toMatchObject({ page: 4, offset: 524_288, length: 90_112 });
    expect(last.text).toContain("line 0008192");
    let collected = "";
    for (let page = 0; page < 5; page += 1) {
      collected += (await preview(backend, "demo-logs", "audit-export.csv", page)).text ?? "";
    }
    expect(collected.length).toBe(614_400);
    expect((await rejection(preview(backend, "demo-logs", "audit-export.csv", 5))).code).toBe("usage");

    expect(await preview(backend, "demo-logs", "empty.txt")).toMatchObject({ kind: "empty", size: 0, text: null });
    expect(await preview(backend, "demo-photos", "documents/report-q3.pdf")).toMatchObject({ kind: "binary", text: null });
    expect(await preview(backend, "demo-logs", "site-photo-large.png")).toMatchObject({
      kind: "too_large",
      media_type: "image/png",
      image_base64: null,
    });
  });

  it("pretty-prints JSON that fits a page and pages the rest as it is", async () => {
    const backend = create();
    const small = await preview(backend, "demo-logs", "config.json");
    expect(small).toMatchObject({ kind: "json", formatted: true, media_type: "application/json", page_count: 1 });
    expect(small.text).toContain('\n  "name": "config.json"');
    const big = await preview(backend, "demo-logs", "config.json", 0, 16);
    expect(big).toMatchObject({ kind: "json", formatted: false, page_count: 24, length: 16 });
  });

  it("gives images as base64 with their type, WebP included", async () => {
    const backend = create();
    const webp = await preview(backend, "demo-logs", "monitoring-dashboard.webp");
    expect(webp).toMatchObject({ kind: "image", media_type: "image/webp", length: 3_072 });
    expect(webp.image_base64?.startsWith("UklGR")).toBe(true);
    const jpg = await preview(backend, "demo-photos", "photos/2024/beach.jpg");
    expect(jpg).toMatchObject({ kind: "image", media_type: "image/png" });
    expect(atob(jpg.image_base64 ?? "").startsWith("\u0089PNG")).toBe(true);
  });

  it("previews one version, refuses delete markers and folders and bad sizes", async () => {
    const backend = create();
    const old = await preview(backend, "demo-photos", "documents/notes.txt", 0, null, "v-notes-3");
    expect(old).toMatchObject({ version_id: "v-notes-3", size: 3_900, kind: "text" });
    expect(old.text).toContain("notes-3 line 0000000");
    const latest = await preview(backend, "demo-photos", "documents/notes.txt");
    expect(latest.text).not.toContain("notes-3");
    expect((await rejection(preview(backend, "demo-photos", "documents/old-draft.txt"))).code).toBe("not_found");
    expect((await rejection(preview(backend, "demo-photos", "documents/old-draft.txt", 0, null, "dm-draft"))).code).toBe("usage");
    expect((await rejection(preview(backend, "demo-photos", "documents/notes.txt", 0, null, "nope"))).code).toBe("not_found");
    expect((await rejection(preview(backend, "demo-photos", "documents/"))).code).toBe("usage");
    expect((await rejection(preview(backend, "demo-photos", "documents/notes.txt", 0, 15))).code).toBe("usage");
    expect((await rejection(preview(backend, "demo-photos", "documents/notes.txt", 0, 1_048_577))).code).toBe("usage");
    expect((await rejection(preview(backend, "demo-photos", "nothing.txt"))).code).toBe("not_found");
  });
});
