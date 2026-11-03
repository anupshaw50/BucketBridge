import { afterEach, describe, expect, it } from "vitest";
import { BackendError } from "../api/errors";
import type { SyncJob, SyncReport } from "../api/types";
import { record, settle } from "../test/helpers";
import { MockBackend } from "./mockBackend";
import { ExcludeRules } from "./syncJobs";
import { MOCK_HOME } from "./world";

const backends: MockBackend[] = [];

function create(): MockBackend {
  const backend = new MockBackend({ autoRun: false, latencyMs: 0, logObjects: 30 });
  backends.push(backend);
  return backend;
}

afterEach(() => {
  while (backends.length > 0) {
    backends.pop()?.dispose();
  }
});

async function demoAccount(backend: MockBackend): Promise<string> {
  const found = (await backend.list_accounts()).accounts.find((account) => account.label === "Demo");
  if (!found) {
    throw new Error("no Demo account");
  }
  return found.id;
}

function job(account: string, name: string): SyncJob {
  return {
    id: "",
    name,
    account_id: account,
    direction: "upload",
    local_root: `${MOCK_HOME}/Pictures/holiday`,
    s3_prefix: "s3://demo-photos/photos/2024/",
    delete: false,
    options: {
      filter: { prefix: null, glob: null, min_size: null, max_size: null, modified_after: null, modified_before: null, storage_classes: [] },
      include: [],
      exclude: [],
      exclude_file: null,
      skip_empty_folders: true,
      skip_duplicate_content: false,
      use_cache: true,
      parallelism: 4,
    },
  };
}

async function code(promise: Promise<unknown>): Promise<string> {
  try {
    await promise;
  } catch (error) {
    if (error instanceof BackendError) {
      return error.code;
    }
    throw error;
  }
  throw new Error("expected a rejection");
}

describe("mock exclusion rules", () => {
  it("follow the gitignore rules of the core", () => {
    const rules = new ExcludeRules(["# comment", "", "*.tmp", "!keep.tmp", "build/", "/top.txt", "docs/old.txt", "logs/**/debug.log"]);
    expect(rules.isExcluded("a.tmp")).toBe(true);
    expect(rules.isExcluded("deep/er/a.tmp")).toBe(true);
    expect(rules.isExcluded("keep.tmp")).toBe(false);
    expect(rules.isExcluded("build/out.bin")).toBe(true);
    expect(rules.isExcluded("x/build/out.bin")).toBe(true);
    expect(rules.isExcluded("rebuild/out.bin")).toBe(false);
    expect(rules.isExcluded("build")).toBe(false);
    expect(rules.isExcluded("top.txt")).toBe(true);
    expect(rules.isExcluded("sub/top.txt")).toBe(false);
    expect(rules.isExcluded("docs/old.txt")).toBe(true);
    expect(rules.isExcluded("x/docs/old.txt")).toBe(false);
    expect(rules.isExcluded("logs/a/b/debug.log")).toBe(true);
    expect(rules.isExcluded("logs/debug.log")).toBe(true);
  });

  it("cannot include a file again below an excluded folder", () => {
    expect(new ExcludeRules(["vendor/", "!vendor/keep.txt"]).isExcluded("vendor/keep.txt")).toBe(true);
    expect(new ExcludeRules(["vendor/*", "!vendor/keep.txt"]).isExcluded("vendor/keep.txt")).toBe(false);
  });
});

describe("mock sync jobs", () => {
  it("stores, lists, replaces and deletes jobs with the backend's validation and events", async () => {
    const backend = create();
    const account = await demoAccount(backend);
    const changed = record(backend, "sync_jobs://changed");
    expect(await backend.list_sync_jobs()).toEqual([]);

    const saved = await backend.save_sync_job({ job: job(account, "  Nightly ") });
    expect(saved.name).toBe("Nightly");
    expect(saved.id).not.toBe("");
    expect(await backend.list_sync_jobs()).toEqual([saved]);

    expect(await code(backend.save_sync_job({ job: job(account, "NIGHTLY") }))).toBe("conflict");
    expect(await code(backend.save_sync_job({ job: job(account, " ") }))).toBe("usage");
    expect(await code(backend.save_sync_job({ job: job("nope", "x") }))).toBe("not_found");
    expect(await code(backend.save_sync_job({ job: { ...job(account, "both"), direction: "bidirectional", delete: true } }))).toBe("usage");
    expect(await code(backend.save_sync_job({ job: { ...saved, id: "missing", name: "Other" } }))).toBe("not_found");

    const renamed = await backend.save_sync_job({ job: { ...saved, name: "Weekly", delete: true } });
    expect(renamed.id).toBe(saved.id);
    expect((await backend.list_sync_jobs()).map((item) => item.name)).toEqual(["Weekly"]);

    expect(await code(backend.delete_sync_job({ id: "missing" }))).toBe("not_found");
    expect(await backend.delete_sync_job({ id: saved.id })).toEqual([]);
    await Promise.resolve();
    expect(changed.length).toBeGreaterThanOrEqual(3);
    expect(await code(backend.run_sync_job({ id: saved.id }))).toBe("not_found");
  });

  it("runs a job with its exclusion rules and sends a report when the plan finishes", async () => {
    const backend = create();
    const account = await demoAccount(backend);
    const reports = record(backend, "sync://report");
    const definition = job(account, "Holiday");
    definition.options.exclude = ["*.cr2"];
    const saved = await backend.save_sync_job({ job: definition });

    const plan = await backend.run_sync_job({ id: saved.id });
    expect(plan.rows.some((row) => row.rel_path.endsWith(".cr2"))).toBe(false);
    expect(plan.rows.some((row) => row.rel_path === "new-photo.jpg")).toBe(true);

    await backend.sync_execute({ plan_id: plan.id });
    backend.advance(10_000);
    await settle();
    const report: SyncReport | undefined = reports.at(-1);
    expect(report).toBeDefined();
    expect(report?.plan_id).toBe(plan.id);
    expect(report?.upload.files).toBeGreaterThan(0);
    expect(report?.upload.bytes).toBeGreaterThan(0);
    expect(report?.failed).toBe(1);
    expect(report?.failures[0]?.rel_path).toBe("flaky-video.mp4");
  });

  it("syncs between two prefixes of the same bucket", async () => {
    const backend = create();
    const account = await demoAccount(backend);
    const reports = record(backend, "sync://report");
    const plan = await backend.sync_analyze({
      account_id: account,
      local_root: "",
      s3_prefix: "s3://demo-photos/copy-of-2024/",
      source_prefix: "s3://demo-photos/photos/2024/",
      direction: "s3_to_s3",
      delete: false,
    });
    expect(plan.direction).toBe("s3_to_s3");
    expect(plan.source_prefix).toBe("s3://demo-photos/photos/2024/");
    expect(plan.rows.length).toBeGreaterThan(0);
    expect(plan.rows.every((row) => row.action === "copy" && row.target === "remote")).toBe(true);
    await backend.sync_execute({ plan_id: plan.id });
    backend.advance(10_000);
    await settle();
    const keys = backend.objectKeys("demo-photos");
    expect(keys.some((key) => key.startsWith("copy-of-2024/"))).toBe(true);
    expect(reports.at(-1)?.copy.files).toBeGreaterThan(0);

    expect(
      await code(
        backend.sync_analyze({
          account_id: account,
          local_root: "",
          s3_prefix: "s3://demo-photos/photos/2024/inner/",
          source_prefix: "s3://demo-photos/photos/2024/",
          direction: "s3_to_s3",
          delete: false,
        }),
      ),
    ).toBe("usage");
  });
});
