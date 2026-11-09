import { afterEach, describe, expect, it } from "vitest";
import { BackendError } from "../api/errors";
import type { UsageProgressEvent } from "../api/types";
import { MockBackend } from "./mockBackend";
import { mockUploadParts } from "./usageMock";

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
  const id = (await backend.list_accounts()).default_account_id;
  if (id === null) {
    throw new Error("no default account");
  }
  return id;
}

async function code(promise: Promise<unknown>): Promise<string> {
  try {
    await promise;
  } catch (raw) {
    if (raw instanceof BackendError) {
      return raw.code;
    }
    throw raw;
  }
  throw new Error("the call did not reject");
}

describe("compute_bucket_usage", () => {
  it("adds up the current objects, a prefix and the unfinished uploads", async () => {
    const backend = create();
    const account_id = await demoAccount(backend);
    const events: UsageProgressEvent[] = [];
    backend.listen("usage://progress", (event) => events.push(event));

    const whole = await backend.compute_bucket_usage({
      account_id,
      bucket: "demo-backups",
      prefix: "",
      versions: false,
      multipart: true,
      job_id: "j1",
    });
    expect(whole.noncurrent).toBeNull();
    expect(whole.objects).toBeGreaterThan(0);
    expect(whole.bytes).toBeGreaterThan(0);

    const uploads = (await backend.list_multipart_uploads({ account_id, buckets: ["demo-backups"] })).uploads;
    const expected = uploads.map((upload) => mockUploadParts(upload.upload_id));
    expect(uploads.length).toBeGreaterThan(0);
    expect(whole.multipart).toEqual({
      uploads: uploads.length,
      parts: expected.reduce((sum, item) => sum + item.parts, 0),
      bytes: expected.reduce((sum, item) => sum + item.bytes, 0),
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(events.length).toBeGreaterThan(0);
    expect(events.every((event) => event.job_id === "j1")).toBe(true);
    expect(events.at(-1)?.phase).toBe("multipart");

    const under = await backend.compute_bucket_usage({
      account_id,
      bucket: "demo-backups",
      prefix: "no-such-prefix/",
      versions: false,
      multipart: false,
      job_id: "j2",
    });
    expect(under).toEqual({ objects: 0, bytes: 0, noncurrent: null, multipart: null });
  });

  it("walks versions and delete markers when asked", async () => {
    const backend = create();
    const account_id = await demoAccount(backend);
    const base = { account_id, bucket: "demo-backups", prefix: "", multipart: false };
    const plain = await backend.compute_bucket_usage({ ...base, versions: false, job_id: "a" });
    const all = await backend.compute_bucket_usage({ ...base, versions: true, job_id: "b" });
    expect(all.noncurrent).not.toBeNull();
    expect(all.objects).toBe(plain.objects);
    expect(all.bytes).toBe(plain.bytes);
  });

  it("rejects bad input, a running job id twice, and a cancelled run", async () => {
    const backend = create();
    const account_id = await demoAccount(backend);
    const base = { account_id, bucket: "demo-backups", prefix: "", versions: false, multipart: false };
    expect(await code(backend.compute_bucket_usage({ ...base, job_id: "" }))).toBe("usage");
    expect(await code(backend.compute_bucket_usage({ ...base, bucket: "bad name", job_id: "x" }))).toBe("usage");

    const first = backend.compute_bucket_usage({ ...base, job_id: "dup" });
    expect(await code(backend.compute_bucket_usage({ ...base, job_id: "dup" }))).toBe("conflict");
    await first;

    const run = backend.compute_bucket_usage({ ...base, job_id: "stop" });
    await backend.cancel_bucket_usage({ job_id: "stop" });
    expect(await code(run)).toBe("cancelled");
    // An unknown job is fine.
    await backend.cancel_bucket_usage({ job_id: "unknown" });
  });
});
