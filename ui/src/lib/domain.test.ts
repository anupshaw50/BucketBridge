import { describe, expect, it } from "vitest";
import type { Account, ObjectEntry, SyncRow, TransferTask } from "../api/types";
import {
  draftFromAccount,
  emptyDraft,
  isEmptyUpdate,
  providerLabel,
  toAccountInput,
  toAccountUpdate,
  validateConnection,
  validateCredentials,
} from "./accounts";
import { filterEntries, nextSort, sortEntries } from "./objects";
import { isFailedRow, parseS3Prefix, pendingRows, syncCounts, visibleRowIndexes } from "./sync";
import { canCancel, canPause, canResume, canRetry, progressText, touchesListing } from "./transfers";

const account: Account = {
  id: "acc-1",
  label: "Work",
  access_key_id: "KEY1",
  secret_ref: "ref/secret",
  session_token_ref: "ref/token",
  region: "eu-west-1",
  endpoint: "https://storage.example.com",
  path_style: true,
  request_payer: false,
  mfa_serial: "arn:aws:iam::000000000000:mfa/dev",
};

describe("account forms", () => {
  it("validates the connection step including unique labels", () => {
    const draft = { ...emptyDraft(), label: " work ", endpoint: "nope", region: "" };
    const errors = validateConnection(draft, [account], null);
    expect(errors.label).toMatch(/already uses/);
    expect(errors.endpoint).toBeDefined();
    expect(errors.region).toBeDefined();
    expect(validateConnection({ ...draft, label: "Work", endpoint: "", region: "us-east-1" }, [account], "acc-1")).toEqual({});
  });

  it("requires a secret when adding, and when the access key changes while editing", () => {
    expect(validateCredentials({ ...emptyDraft(), access_key_id: "K" }, null).secret_access_key).toBeDefined();
    const edit = draftFromAccount(account);
    expect(validateCredentials(edit, account)).toEqual({});
    expect(validateCredentials({ ...edit, access_key_id: "OTHER" }, account).secret_access_key).toMatch(/new access key/);
    expect(validateCredentials({ ...edit, session_token_mode: "replace" }, account).session_token).toBeDefined();
  });

  it("trims input and turns blanks into null", () => {
    const input = toAccountInput({
      ...emptyDraft(),
      label: " Work ",
      access_key_id: " K ",
      secret_access_key: " S ",
      endpoint: "  ",
      mfa_serial: "",
    });
    expect(input).toMatchObject({ label: "Work", access_key_id: "K", secret_access_key: "S", endpoint: null, mfa_serial: null });
  });

  it("builds a minimal update with keep, rotate and clear semantics", () => {
    const unchanged = toAccountUpdate(account, draftFromAccount(account));
    expect(isEmptyUpdate(unchanged)).toBe(true);
    const update = toAccountUpdate(account, {
      ...draftFromAccount(account),
      secret_access_key: "new-secret",
      endpoint: "",
      mfa_serial: "",
      session_token_mode: "clear",
      region: "us-west-2",
    });
    expect(update).toEqual({
      label: null,
      access_key_id: null,
      secret_access_key: "new-secret",
      session_token: null,
      clear_session_token: true,
      region: "us-west-2",
      endpoint: null,
      clear_endpoint: true,
      path_style: null,
      request_payer: null,
      mfa_serial: null,
      clear_mfa_serial: true,
    });
    expect(providerLabel(account)).toBe("storage.example.com");
    expect(providerLabel({ ...account, endpoint: null })).toBe("AWS");
  });
});

function entry(key: string, size = 0, modified: string | null = null, prefix = key.endsWith("/")): ObjectEntry {
  return {
    key,
    size,
    etag: null,
    last_modified: modified,
    storage_class: prefix ? null : "STANDARD",
    checksums: { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null },
    is_prefix: prefix,
  };
}

describe("object sorting and search", () => {
  const entries = [
    entry("p/file10.txt", 30, "2026-10-03T00:00:00Z"),
    entry("p/b/"),
    entry("p/file2.txt", 10, "2026-10-01T00:00:00Z"),
    entry("p/A/"),
    entry("p/marker/", 0, "2026-10-02T00:00:00Z", false),
  ];

  it("keeps folders first and sorts names naturally", () => {
    const sorted = sortEntries(entries, { column: "name", direction: "asc" }, "p/").map((item) => item.key);
    expect(sorted).toEqual(["p/A/", "p/b/", "p/marker/", "p/file2.txt", "p/file10.txt"]);
    const reversed = sortEntries(entries, { column: "name", direction: "desc" }, "p/").map((item) => item.key);
    expect(reversed).toEqual(["p/marker/", "p/b/", "p/A/", "p/file10.txt", "p/file2.txt"]);
  });

  it("sorts by size and date with folders still first", () => {
    const bySize = sortEntries(entries, { column: "size", direction: "desc" }, "p/").map((item) => item.key);
    expect(bySize.slice(3)).toEqual(["p/file10.txt", "p/file2.txt"]);
    const byDate = sortEntries(entries, { column: "modified", direction: "asc" }, "p/").map((item) => item.key);
    expect(byDate.slice(3)).toEqual(["p/file2.txt", "p/file10.txt"]);
    expect(nextSort({ column: "size", direction: "asc" }, "size")).toEqual({ column: "size", direction: "desc" });
    expect(nextSort({ column: "size", direction: "desc" }, "name")).toEqual({ column: "name", direction: "asc" });
  });

  it("filters by name case-insensitively", () => {
    expect(filterEntries(entries, "FILE1", "p/").map((item) => item.key)).toEqual(["p/file10.txt"]);
    expect(filterEntries(entries, "  ", "p/")).toHaveLength(5);
  });
});

function task(state: TransferTask["state"], kind: TransferTask["kind"] = "upload"): TransferTask {
  return {
    id: "t",
    account_id: "acc-1",
    group_id: null,
    kind,
    src: kind === "upload" ? { type: "local", path: "/tmp/a" } : { type: "s3", bucket: "b", key: "dir/a" },
    dst: kind === "delete" ? null : { type: "s3", bucket: "b", key: "dir/a" },
    bytes_total: 2048,
    bytes_done: 1024,
    speed: 0,
    eta: null,
    state,
    error: null,
    attempts: 1,
    created_at: "2026-10-29T08:00:00Z",
    updated_at: "2026-10-29T08:00:00Z",
  };
}

describe("transfer rules", () => {
  it("enables per-task actions only in valid states", () => {
    expect(canPause("queued") && canPause("running")).toBe(true);
    expect(canPause("stopped") || canPause("failed") || canPause("completed")).toBe(false);
    expect(canResume("stopped")).toBe(true);
    expect(canResume("failed")).toBe(false);
    expect(canRetry("failed")).toBe(true);
    expect(canRetry("cancelled")).toBe(false);
    expect(canCancel("failed") && canCancel("stopped")).toBe(true);
    expect(canCancel("completed") || canCancel("cancelled")).toBe(false);
  });

  it("describes progress in bytes or objects", () => {
    expect(progressText(task("running"))).toBe("1.0 KiB of 2.0 KiB");
    expect(progressText({ ...task("running", "delete"), bytes_total: 10, bytes_done: 3 })).toBe("3 of 10 objects");
  });

  it("detects tasks that change the open folder", () => {
    expect(touchesListing(task("completed"), "acc-1", "b", "dir/")).toBe(true);
    expect(touchesListing(task("completed"), "acc-2", "b", "dir/")).toBe(false);
    expect(touchesListing(task("completed", "download"), "acc-1", "b", "dir/")).toBe(false);
    expect(touchesListing(task("completed", "delete"), "acc-1", "b", "")).toBe(true);
    expect(touchesListing(task("completed"), "acc-1", "b", "other/")).toBe(false);
  });
});

function row(action: SyncRow["action"], done: boolean, error: string | null = null): SyncRow {
  return { rel_path: action, action, target: null, local_meta: null, s3_meta: null, error, done };
}

describe("sync helpers", () => {
  it("counts actions and finds failed rows", () => {
    const rows = [
      row("copy", false),
      row("equal", true),
      row("failed", false, "denied"),
      row("copy", false, "reset"),
      row("delete", true),
    ];
    expect(syncCounts(rows)).toEqual({ copy: 2, delete: 1, equal: 1, failed: 1 });
    expect(rows.map(isFailedRow)).toEqual([false, false, true, true, false]);
    expect(visibleRowIndexes(rows, "failed")).toEqual([2, 3]);
    expect(visibleRowIndexes(rows, "all")).toEqual([0, 1, 2, 3, 4]);
    // Execute runs not-done copy and delete rows only; failed rows cannot be executed.
    expect(pendingRows(rows)).toBe(2);
  });

  it("parses s3 prefixes", () => {
    expect(parseS3Prefix("s3://bucket/a/b/")).toEqual({ bucket: "bucket", prefix: "a/b/" });
    expect(parseS3Prefix("s3://bucket/")).toEqual({ bucket: "bucket", prefix: "" });
    expect(parseS3Prefix("s3://bucket/a")).toBeNull();
    expect(parseS3Prefix("https://bucket/")).toBeNull();
  });
});
