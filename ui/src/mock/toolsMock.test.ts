import { afterEach, describe, expect, it } from "vitest";
import { BackendError } from "../api/errors";
import type { DiagnosticsReport, FilterSpec, SavedFilter } from "../api/types";
import { emptyFilterSpec } from "../lib/filters";
import { DEFAULT_SETTINGS } from "../lib/settings";
import { MockBackend, type MockOptions } from "./mockBackend";
import { csvField, csvText } from "./toolsMock";
import { MOCK_CREDENTIALS, MOCK_HOME, MOCK_MFA_CODE } from "./world";

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
  const found = (await backend.list_accounts()).accounts.find((account) => account.label === label);
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

function step(report: DiagnosticsReport, id: DiagnosticsReport["steps"][number]["id"]) {
  const found = report.steps.find((item) => item.id === id);
  if (!found) {
    throw new Error(`no step ${id}`);
  }
  return found;
}

function input(patch: Partial<{ access: string; secret: string; endpoint: string | null; region: string }> = {}) {
  return {
    label: "Unsaved",
    access_key_id: patch.access ?? MOCK_CREDENTIALS.demo.accessKeyId,
    secret_access_key: patch.secret ?? MOCK_CREDENTIALS.demo.secret,
    session_token: null,
    region: patch.region ?? "us-east-1",
    endpoint: patch.endpoint ?? null,
    path_style: false,
    request_payer: false,
    mfa_serial: null,
  };
}

function spec(patch: Partial<FilterSpec>): FilterSpec {
  return { ...emptyFilterSpec(), ...patch };
}

function saved(name: string, patch: Partial<FilterSpec> = {}): SavedFilter {
  return { name, filter: spec(patch) };
}

describe("mock diagnostics", () => {
  it("probes a stored account with all six steps in order", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const report = await backend.run_diagnostics({ account_id: id, input: null, bucket: null });
    expect(report.steps.map((item) => item.id)).toEqual(["dns", "tcp", "proxy", "tls", "credentials", "region"]);
    expect(report.steps.map((item) => item.status)).toEqual(["pass", "pass", "skip", "pass", "pass", "skip"]);
    expect(report.steps.map((item) => item.code)).toEqual(["resolved", "connected", "none", "handshake", "valid", "no_bucket"]);
    expect(report.ok).toBe(true);
    expect(report.target).toBe("https://s3.us-east-1.amazonaws.com");
    expect(report.region).toBe("us-east-1");
    expect(report.bucket).toBeNull();
    expect(Number.isNaN(Date.parse(report.started_at))).toBe(false);
    expect(report.steps.filter((item) => item.status === "skip").every((item) => item.elapsed_ms === 0)).toBe(true);
    expect(report.elapsed_ms).toBeGreaterThanOrEqual(report.steps.reduce((sum, item) => sum + item.elapsed_ms, 0));
  });

  it("is deterministic for one setup and never carries a secret", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const first = await backend.run_diagnostics({ account_id: id, input: null, bucket: "demo-photos" });
    const second = await backend.run_diagnostics({ account_id: id, input: null, bucket: "demo-photos" });
    expect(second.steps).toEqual(first.steps);
    const text = JSON.stringify(first);
    expect(text).not.toContain(MOCK_CREDENTIALS.demo.secret);
    const unsaved = await backend.run_diagnostics({ account_id: null, input: input({ secret: "typed-secret-value" }), bucket: null });
    expect(JSON.stringify(unsaved)).not.toContain("typed-secret-value");
  });

  it("probes the region of a named bucket", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const probe = (bucket: string) => backend.run_diagnostics({ account_id: id, input: null, bucket });
    const home = step(await probe("demo-photos"), "region");
    expect([home.status, home.code]).toEqual(["pass", "found"]);
    expect(home.summary).toBe("The bucket demo-photos is in us-east-1.");
    const abroad = step(await probe("demo-backups"), "region");
    expect([abroad.status, abroad.code]).toEqual(["pass", "found"]);
    expect(abroad.detail).toContain("eu-west-1");
    const foreign = step(await probe("secure-vault"), "region");
    expect([foreign.status, foreign.code]).toEqual(["fail", "access_denied"]);
    const missing = step(await probe("no-such-bucket"), "region");
    expect([missing.status, missing.code]).toEqual(["fail", "bucket_not_found"]);
    const bad = await rejection(probe("Not A Bucket"));
    expect(bad.code).toBe("usage");
  });

  it("reports credential problems as results, not rejections", async () => {
    const backend = create();
    const wrong = await backend.run_diagnostics({ account_id: await accountId(backend, "Wrong secret"), input: null, bucket: "demo-photos" });
    expect(wrong.ok).toBe(false);
    expect(step(wrong, "credentials")).toMatchObject({ status: "fail", code: "bad_signature" });
    // The region probe still answers: the bucket is out of reach with rejected credentials.
    expect(step(wrong, "region")).toMatchObject({ status: "fail", code: "access_denied" });
    const unknown = await backend.run_diagnostics({ account_id: null, input: input({ access: "AKIAUNKNOWNKEY00001" }), bucket: null });
    expect(step(unknown, "credentials")).toMatchObject({ status: "fail", code: "invalid_access_key" });
    expect(unknown.ok).toBe(false);
  });

  it("probes an unsaved account without storing it", async () => {
    const backend = create();
    const before = (await backend.list_accounts()).accounts.length;
    const report = await backend.run_diagnostics({ account_id: null, input: input({ access: MOCK_CREDENTIALS.fresh.accessKeyId, secret: MOCK_CREDENTIALS.fresh.secret }), bucket: null });
    expect(report.ok).toBe(true);
    expect((await backend.list_accounts()).accounts).toHaveLength(before);
  });

  it("follows the endpoint: TLS for https, refused or unresolvable hosts block the later steps", async () => {
    const backend = create();
    const custom = await backend.run_diagnostics({
      account_id: null,
      input: input({ access: MOCK_CREDENTIALS.external.accessKeyId, secret: MOCK_CREDENTIALS.external.secret, endpoint: "https://storage.example.com", region: "eu-central-1" }),
      bucket: "archive-2025",
    });
    expect(custom.target).toBe("https://storage.example.com");
    expect(step(custom, "tls")).toMatchObject({ status: "pass", code: "handshake" });
    expect(step(custom, "region")).toMatchObject({ status: "pass", code: "no_region_header" });

    const local = await backend.run_diagnostics({ account_id: null, input: input({ endpoint: "http://127.0.0.1:9000" }), bucket: "demo-photos" });
    expect(local.steps.map((item) => [item.id, item.status, item.code])).toEqual([
      ["dns", "skip", "ip_address"],
      ["tcp", "fail", "refused"],
      ["proxy", "skip", "blocked"],
      ["tls", "skip", "blocked"],
      ["credentials", "skip", "blocked"],
      ["region", "skip", "blocked"],
    ]);
    expect(local.ok).toBe(false);

    const nowhere = await backend.run_diagnostics({ account_id: null, input: input({ endpoint: "https://nowhere.example.org" }), bucket: null });
    expect(step(nowhere, "dns")).toMatchObject({ status: "fail", code: "not_found" });
    expect(step(nowhere, "tcp").code).toBe("blocked");
    expect(step(nowhere, "credentials").code).toBe("blocked");

    const plain = await backend.run_diagnostics({
      account_id: null,
      input: input({ access: MOCK_CREDENTIALS.external.accessKeyId, secret: MOCK_CREDENTIALS.external.secret, endpoint: "https://storage.example.com" }),
      bucket: null,
    });
    expect(step(plain, "proxy").code).toBe("none");
  });

  it("describes the proxy when one carries the traffic, unless the host is bypassed", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const settings = structuredClone(DEFAULT_SETTINGS);
    settings.proxy = { ...settings.proxy, enabled: true, kind: "socks5", host: "proxy.example.net", port: 1080 };
    await backend.save_settings({ settings });
    const proxied = await backend.run_diagnostics({ account_id: id, input: null, bucket: null });
    expect(step(proxied, "dns").summary).toContain("proxy.example.net");
    expect(step(proxied, "tcp").summary).toContain(":1080");
    expect(step(proxied, "proxy")).toMatchObject({ status: "pass", code: "tunnel_open" });
    expect(step(proxied, "proxy").summary).toContain("SOCKS5");
    expect(JSON.stringify(proxied)).not.toContain("password");

    settings.proxy.bypass = ["*.amazonaws.com"];
    await backend.save_settings({ settings });
    const bypassed = await backend.run_diagnostics({ account_id: id, input: null, bucket: null });
    expect(step(bypassed, "proxy")).toMatchObject({ status: "skip", code: "direct" });
    expect(step(bypassed, "dns").summary).toContain("s3.us-east-1.amazonaws.com");
  });

  it("validates its arguments and applies the lock and MFA rules of stored accounts", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    expect((await rejection(backend.run_diagnostics({ account_id: null, input: null, bucket: null }))).code).toBe("usage");
    expect((await rejection(backend.run_diagnostics({ account_id: id, input: input(), bucket: null }))).code).toBe("usage");
    expect((await rejection(backend.run_diagnostics({ account_id: "missing", input: null, bucket: null }))).code).toBe("not_found");
    expect((await rejection(backend.run_diagnostics({ account_id: null, input: input({ region: "Not A Region" }), bucket: null }))).code).toBe("usage");
    expect((await rejection(backend.run_diagnostics({ account_id: null, input: input({ endpoint: "ftp://x" }), bucket: null }))).code).toBe("usage");
    expect((await rejection(backend.run_diagnostics({ account_id: null, input: { ...input(), secret_access_key: "" }, bucket: null }))).code).toBe("usage");

    const secure = await accountId(backend, "Secure (MFA)");
    expect((await rejection(backend.run_diagnostics({ account_id: secure, input: null, bucket: null }))).code).toBe("mfa_required");
    await backend.mfa_login({ account_id: secure, code: MOCK_MFA_CODE });
    expect((await backend.run_diagnostics({ account_id: secure, input: null, bucket: null })).ok).toBe(true);

    const locked = create({ masterPassword: "correct horse", locked: true });
    const lockedId = (await locked.list_accounts()).accounts[0]?.id ?? "";
    expect((await rejection(locked.run_diagnostics({ account_id: lockedId, input: null, bucket: null }))).code).toBe("locked");
  });

  it("logs how a run ended", async () => {
    const backend = create();
    const id = await accountId(backend, "Wrong secret");
    await backend.run_diagnostics({ account_id: id, input: null, bucket: null });
    const entries = await backend.get_log({ levels: null, text: "Diagnostics finished", limit: null });
    expect(entries.map((entry) => [entry.level, entry.message])).toEqual([["warning", "Diagnostics finished: 1 check(s) failed."]]);
  });
});

describe("mock multipart uploads", () => {
  it("lists the uploads of the chosen buckets, sorted, with failures per bucket", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const backups = await backend.list_multipart_uploads({ account_id: id, buckets: ["demo-backups"] });
    expect(backups.failures).toEqual([]);
    expect(backups.uploads.map((row) => row.key)).toEqual(["archive/2018.tar", "archive/2019.tar", "db/2026-10-29.sql.gz"]);
    expect(backups.uploads.every((row) => row.bucket === "demo-backups" && row.upload_id.length > 8)).toBe(true);
    expect(backups.uploads.every((row) => row.initiated !== null && row.storage_class !== null)).toBe(true);

    const many = await backend.list_multipart_uploads({
      account_id: id,
      buckets: ["demo-website", "demo-photos", "demo-backups", "demo-empty", "no-such-bucket", "demo-photos"],
    });
    expect(many.uploads.map((row) => `${row.bucket}/${row.key}`)).toEqual([
      "demo-backups/archive/2018.tar",
      "demo-backups/archive/2019.tar",
      "demo-backups/db/2026-10-29.sql.gz",
      "demo-photos/photos/2024/raw/IMG_0001.cr2",
      "demo-photos/photos/2025/winter/video.mov",
    ]);
    expect(many.failures.map((failure) => [failure.bucket, failure.error.code])).toEqual([
      ["demo-website", "auth"],
      ["no-such-bucket", "not_found"],
    ]);
    expect(many.failures[0]?.error.message).toContain("AccessDenied");
  });

  it("has uploads older and younger than a month", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const { uploads } = await backend.list_multipart_uploads({ account_id: id, buckets: ["demo-backups", "demo-photos", "demo-logs"] });
    const ages = uploads.map((row) => (Date.now() - Date.parse(row.initiated ?? "")) / 86_400_000);
    expect(ages.some((age) => age < 30)).toBe(true);
    expect(ages.some((age) => age > 30)).toBe(true);
  });

  it("validates the buckets and the account", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    expect((await rejection(backend.list_multipart_uploads({ account_id: id, buckets: [] }))).code).toBe("usage");
    expect((await rejection(backend.list_multipart_uploads({ account_id: id, buckets: ["Bad Name"] }))).code).toBe("usage");
    expect((await rejection(backend.list_multipart_uploads({ account_id: "missing", buckets: ["demo-photos"] }))).code).toBe("not_found");
    const secure = await accountId(backend, "Secure (MFA)");
    expect((await rejection(backend.list_multipart_uploads({ account_id: secure, buckets: ["secure-vault"] }))).code).toBe("mfa_required");
    // Another account's server has its own buckets.
    const wrong = await accountId(backend, "Wrong secret");
    const denied = await backend.list_multipart_uploads({ account_id: wrong, buckets: ["demo-photos"] });
    expect(denied.uploads).toEqual([]);
    expect(denied.failures[0]?.error.code).toBe("auth");
  });

  it("aborts exactly the named uploads and reports each result in order", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const { uploads } = await backend.list_multipart_uploads({ account_id: id, buckets: ["demo-backups"] });
    const [first, second, third] = uploads;
    if (!first || !second || !third) {
      throw new Error("expected three uploads");
    }
    const ref = (row: typeof first) => ({ bucket: row.bucket, key: row.key, upload_id: row.upload_id });
    const results = await backend.abort_multipart_uploads({ account_id: id, uploads: [ref(third), ref(first)] });
    expect(results.map((result) => [result.key, result.aborted, result.error])).toEqual([
      [third.key, true, null],
      [first.key, true, null],
    ]);
    const left = await backend.list_multipart_uploads({ account_id: id, buckets: ["demo-backups"] });
    expect(left.uploads.map((row) => row.key)).toEqual([second.key]);

    // An upload that is gone is a result, not a rejection, and the others still run.
    const again = await backend.abort_multipart_uploads({ account_id: id, uploads: [ref(first), ref(second)] });
    expect(again[0]).toMatchObject({ aborted: false, error: { code: "not_found" } });
    expect(again[1]).toMatchObject({ aborted: true, error: null });
    expect((await backend.list_multipart_uploads({ account_id: id, buckets: ["demo-backups"] })).uploads).toEqual([]);
    const log = await backend.get_log({ levels: null, text: "Aborted upload", limit: null });
    expect(log).toHaveLength(3);
  });

  it("rejects an empty or invalid abort request and reports inaccessible buckets per upload", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    expect((await rejection(backend.abort_multipart_uploads({ account_id: id, uploads: [] }))).code).toBe("usage");
    expect((await rejection(backend.abort_multipart_uploads({ account_id: id, uploads: [{ bucket: "Bad", key: "k", upload_id: "u" }] }))).code).toBe("usage");
    expect((await rejection(backend.abort_multipart_uploads({ account_id: id, uploads: [{ bucket: "demo-photos", key: "", upload_id: "u" }] }))).code).toBe("usage");
    expect((await rejection(backend.abort_multipart_uploads({ account_id: id, uploads: [{ bucket: "demo-photos", key: "k", upload_id: "" }] }))).code).toBe("usage");
    const results = await backend.abort_multipart_uploads({
      account_id: id,
      uploads: [
        { bucket: "no-such-bucket", key: "k", upload_id: "u" },
        { bucket: "secure-vault", key: "k", upload_id: "u" },
      ],
    });
    expect(results.map((result) => result.error?.code)).toEqual(["not_found", "auth"]);
    expect(results.every((result) => !result.aborted)).toBe(true);
  });
});

describe("mock saved filters", () => {
  it("adds, lists, replaces, renames and deletes filters", async () => {
    const backend = create();
    expect(await backend.list_filters()).toEqual([]);
    const first = saved("Big logs", { prefix: "logs/", min_size: 1024 });
    expect(await backend.save_filter({ filter: first, replaces: null })).toEqual([first]);
    const second = saved("Images", { glob: "**/*.{jpg,png}" });
    expect(await backend.save_filter({ filter: second, replaces: null })).toEqual([first, second]);
    // Replace in place, with a rename.
    const renamed = saved("Large logs", { prefix: "logs/", min_size: 2048 });
    expect(await backend.save_filter({ filter: renamed, replaces: "big LOGS" })).toEqual([renamed, second]);
    expect(await backend.list_filters()).toEqual([renamed, second]);
    expect(await backend.delete_filter({ name: "IMAGES" })).toEqual([renamed]);
    expect(await backend.list_filters()).toEqual([renamed]);
  });

  it("keeps names unique ignoring case and trims them", async () => {
    const backend = create();
    await backend.save_filter({ filter: saved("  Logs  "), replaces: null });
    expect((await backend.list_filters())[0]?.name).toBe("Logs");
    expect((await rejection(backend.save_filter({ filter: saved("LOGS"), replaces: null }))).code).toBe("conflict");
    await backend.save_filter({ filter: saved("Other"), replaces: null });
    expect((await rejection(backend.save_filter({ filter: saved("logs"), replaces: "Other" }))).code).toBe("conflict");
    // Keeping the own name, in another case, is a plain replacement.
    expect((await backend.save_filter({ filter: saved("OTHER"), replaces: "Other" })).map((item) => item.name)).toEqual(["Logs", "OTHER"]);
    expect((await rejection(backend.save_filter({ filter: saved("x"), replaces: "missing" }))).code).toBe("not_found");
    expect((await rejection(backend.delete_filter({ name: "missing" }))).code).toBe("not_found");
  });

  it("validates names and specs with the filter engine", async () => {
    const backend = create();
    const usage = async (filter: SavedFilter) => (await rejection(backend.save_filter({ filter, replaces: null }))).code;
    expect(await usage(saved(""))).toBe("usage");
    expect(await usage(saved("   "))).toBe("usage");
    expect(await usage(saved("x".repeat(101)))).toBe("usage");
    expect(await usage(saved("tab\tname"))).toBe("usage");
    expect(await usage(saved("bad glob", { glob: "[a" }))).toBe("usage");
    expect(await usage(saved("bad sizes", { min_size: 5, max_size: 1 }))).toBe("usage");
    expect(await usage(saved("bad dates", { modified_after: "2026-02-01T00:00:00Z", modified_before: "2026-01-01T00:00:00Z" }))).toBe("usage");
    expect(await usage(saved("bad class", { storage_classes: [" "] }))).toBe("usage");
    expect(await backend.list_filters()).toEqual([]);
    // 100 characters are fine.
    expect(await backend.save_filter({ filter: saved("y".repeat(100)), replaces: null })).toHaveLength(1);
  });

  it("stores at most 200 filters and hands out copies", async () => {
    const backend = create();
    for (let index = 0; index < 200; index += 1) {
      await backend.save_filter({ filter: saved(`filter ${index}`), replaces: null });
    }
    expect((await rejection(backend.save_filter({ filter: saved("one too many"), replaces: null }))).code).toBe("conflict");
    const list = await backend.list_filters();
    expect(list).toHaveLength(200);
    list.length = 0;
    expect(await backend.list_filters()).toHaveLength(200);
  });
});

describe("mock export", () => {
  const OUT = `${MOCK_HOME}/Documents`;

  async function exportOf(backend: MockBackend, patch: Partial<Parameters<MockBackend["export_listing"]>[0]> = {}) {
    const id = await accountId(backend, "Demo");
    return backend.export_listing({
      account_id: id,
      bucket: "demo-photos",
      prefix: "",
      recursive: false,
      format: "csv",
      path: `${OUT}/listing.csv`,
      filter: null,
      ...patch,
    });
  }

  it("writes one level (folders first) or everything below the prefix", async () => {
    const backend = create();
    const folder = await exportOf(backend);
    expect(folder.rows).toBe(7);
    expect(folder.path).toBe(`${OUT}/listing.csv`);
    expect(backend.fs.stat(`${OUT}/listing.csv`)?.size).toBe(folder.bytes);
    const everything = await exportOf(backend, { recursive: true, path: `${OUT}/all.csv` });
    expect(everything.rows).toBe(13);
    expect(everything.bytes).toBeGreaterThan(folder.bytes);
    const inner = await exportOf(backend, { prefix: "documents/", recursive: false, path: `${OUT}/documents.csv` });
    expect(inner.rows).toBe(3);
  });

  it("sizes a CSV file like the text it stands for", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const page = await backend.list_objects({ account_id: id, bucket: "demo-website", prefix: "", flat: true, token: null });
    const outcome = await exportOf(backend, { bucket: "demo-website", recursive: true, path: `${OUT}/site.csv` });
    expect(outcome.rows).toBe(page.entries.length);
    expect(outcome.bytes).toBe(new TextEncoder().encode(csvText(page.entries)).length);
    expect(csvText(page.entries).split("\n")[0]).toBe("key,size,last_modified,etag,storage_class");
    expect(csvText(page.entries)).toContain("2026-10-13T09:30:00Z");
  });

  it("estimates the binary formats from the row count", async () => {
    const backend = create();
    const xlsx = await exportOf(backend, { format: "xlsx", path: `${OUT}/a.xlsx` });
    const parquet = await exportOf(backend, { format: "parquet", path: `${OUT}/a.parquet` });
    expect(xlsx.rows).toBe(parquet.rows);
    expect(xlsx.bytes).toBeGreaterThan(1000);
    expect(parquet.bytes).toBeGreaterThan(1000);
    expect(backend.fs.isFile(`${OUT}/a.xlsx`) && backend.fs.isFile(`${OUT}/a.parquet`)).toBe(true);
  });

  it("keeps only the rows a filter accepts, relative to the prefix", async () => {
    const backend = create();
    const texts = await exportOf(backend, { prefix: "documents/", filter: spec({ glob: "*.txt" }), path: `${OUT}/t.csv` });
    expect(texts.rows).toBe(1);
    const jpgs = await exportOf(backend, { recursive: true, filter: spec({ glob: "**/*.jpg" }), path: `${OUT}/j.csv` });
    // Four JPEGs, and the two zero-byte folder markers, which a filter keeps like folders.
    expect(jpgs.rows).toBe(6);
    expect((await rejection(exportOf(backend, { filter: spec({ glob: "[a" }), path: `${OUT}/bad.csv` }))).code).toBe("usage");
  });

  it("replaces an existing file and refuses paths it cannot write", async () => {
    const backend = create();
    const first = await exportOf(backend, { recursive: true });
    const second = await exportOf(backend, { recursive: false });
    expect(second.bytes).not.toBe(first.bytes);
    expect(backend.fs.stat(`${OUT}/listing.csv`)?.size).toBe(second.bytes);
    expect((await rejection(exportOf(backend, { path: "listing.csv" }))).code).toBe("usage");
    expect((await rejection(exportOf(backend, { path: `${OUT}` }))).code).toBe("io");
    expect((await rejection(exportOf(backend, { path: "/no/such/folder/listing.csv" }))).code).toBe("io");
    expect((await rejection(exportOf(backend, { bucket: "no-such-bucket" }))).code).toBe("not_found");
    expect((await rejection(exportOf(backend, { bucket: "secure-vault" }))).code).toBe("auth");
  });

  it("records the export in the event log", async () => {
    const backend = create();
    await exportOf(backend);
    const log = await backend.get_log({ levels: null, text: "Exported 7 row(s)", limit: null });
    expect(log).toHaveLength(1);
    expect(log[0]?.message).toContain(`${OUT}/listing.csv as csv`);
  });
});

describe("CSV text", () => {
  it("defuses formulas and quotes what needs it", () => {
    expect(csvField("plain")).toBe("plain");
    expect(csvField("=HYPERLINK(1)")).toBe("'=HYPERLINK(1)");
    expect(csvField("+1")).toBe("'+1");
    expect(csvField("-1")).toBe("'-1");
    expect(csvField("@sum")).toBe("'@sum");
    expect(csvField("\tx")).toBe("'\tx");
    expect(csvField("a,b")).toBe('"a,b"');
    expect(csvField('say "hi"')).toBe('"say ""hi"""');
    expect(csvField("two\nlines")).toBe('"two\nlines"');
    expect(csvField("=a,b")).toBe('"\'=a,b"');
  });

  it("leaves size, time, etag and class empty for folders and unknown values", () => {
    const text = csvText([
      { key: "dir/", size: 0, etag: null, last_modified: null, storage_class: null, checksums: { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null }, is_prefix: true },
      { key: "a.txt", size: 5, etag: "abc", last_modified: "2026-10-29T08:15:00.000Z", storage_class: "STANDARD", checksums: { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null }, is_prefix: false },
      { key: "b.txt", size: 6, etag: null, last_modified: "2026-10-29T08:15:00.123Z", storage_class: null, checksums: { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null }, is_prefix: false },
    ]);
    expect(text).toBe(
      [
        "key,size,last_modified,etag,storage_class",
        "dir/,,,,",
        "a.txt,5,2026-10-29T08:15:00Z,abc,STANDARD",
        "b.txt,6,2026-10-29T08:15:00.123Z,,",
        "",
      ].join("\n"),
    );
  });
});
