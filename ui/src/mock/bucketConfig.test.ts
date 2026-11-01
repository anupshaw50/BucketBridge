import { afterEach, describe, expect, it } from "vitest";
import { BackendError } from "../api/errors";
import type { WebsiteConfig } from "../api/types";
import { MockBackend, type MockOptions } from "./mockBackend";

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

const POLICY = `{
  "Version": "2012-10-17",
  "Statement": [{"Effect": "Allow", "Principal": "*", "Action": "s3:GetObject", "Resource": "arn:aws:s3:::demo-empty/*"}]
}`;

describe("mock bucket configuration", () => {
  it("round-trips every section of a bucket: read, write, read again", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const ref = { account_id: id, bucket: "demo-empty" };

    // Nothing configured is an answer.
    expect(await backend.get_bucket_policy(ref)).toBeNull();
    expect(await backend.get_public_access_block(ref)).toBeNull();
    expect(await backend.get_bucket_versioning(ref)).toEqual({ status: "unversioned", mfa_delete: false });
    expect(await backend.get_bucket_logging(ref)).toBeNull();
    expect(await backend.get_bucket_website(ref)).toBeNull();
    expect(await backend.get_bucket_acceleration(ref)).toBe("unset");

    await backend.put_bucket_policy({ ...ref, policy: POLICY });
    const read = await backend.get_bucket_policy(ref);
    expect(read).toContain('\n  "Statement": [\n    {\n      "Effect": "Allow",');
    expect(JSON.parse(read ?? "")).toEqual(JSON.parse(POLICY));

    const block = {
      block_public_acls: true,
      ignore_public_acls: false,
      block_public_policy: true,
      restrict_public_buckets: false,
    };
    await backend.put_public_access_block({ ...ref, config: block });
    expect(await backend.get_public_access_block(ref)).toEqual(block);

    await backend.put_bucket_versioning({ ...ref, status: "enabled" });
    expect(await backend.get_bucket_versioning(ref)).toEqual({ status: "enabled", mfa_delete: false });

    const target = { target_bucket: "demo-logs", target_prefix: "empty/" };
    await backend.put_bucket_logging({ ...ref, target });
    expect(await backend.get_bucket_logging(ref)).toEqual(target);

    const website: WebsiteConfig = {
      type: "hosting",
      index_document: "index.html",
      error_document: "error.html",
      routing_rules: [],
    };
    await backend.put_bucket_website({ ...ref, website });
    expect(await backend.get_bucket_website(ref)).toEqual(website);

    await backend.put_bucket_acceleration({ ...ref, status: "enabled" });
    expect(await backend.get_bucket_acceleration(ref)).toBe("enabled");

    // The combined read shows all of it.
    const config = await backend.get_bucket_config(ref);
    expect(config.public_access_block).toEqual({ type: "loaded", value: block });
    expect(config.logging).toEqual({ type: "loaded", value: target });
    expect(config.website).toEqual({ type: "loaded", value: website });
    expect(config.acceleration).toEqual({ type: "loaded", value: "enabled" });

    // Removing works, and removing again is fine.
    await backend.delete_bucket_policy(ref);
    await backend.delete_bucket_policy(ref);
    expect(await backend.get_bucket_policy(ref)).toBeNull();
    await backend.delete_public_access_block(ref);
    await backend.delete_public_access_block(ref);
    expect(await backend.get_public_access_block(ref)).toBeNull();
    await backend.delete_bucket_website(ref);
    await backend.delete_bucket_website(ref);
    expect(await backend.get_bucket_website(ref)).toBeNull();
    await backend.disable_bucket_logging(ref);
    expect(await backend.get_bucket_logging(ref)).toBeNull();
  });

  it("rejects invalid input with usage errors that name every problem", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const ref = { account_id: id, bucket: "demo-empty" };
    const bad = await rejection(backend.put_bucket_policy({ ...ref, policy: '{"Version":"2012-10-17","Statement":[{"Effect":"x"}]}' }));
    expect(bad.code).toBe("usage");
    expect(bad.message).toMatch(/^invalid bucket policy: Statement\[0\]\.Effect: must be "Allow" or "Deny" \(found "x"\); /);
    expect(bad.message).toContain('needs "Principal" or "NotPrincipal"');
    expect((await rejection(backend.put_bucket_versioning({ ...ref, status: "unversioned" }))).code).toBe("usage");
    expect((await rejection(backend.put_bucket_acceleration({ ...ref, status: "unset" }))).code).toBe("usage");
    expect(
      (await rejection(backend.put_bucket_website({ ...ref, website: { type: "redirect", host_name: "", protocol: null } }))).message,
    ).toBe("invalid website configuration: the redirect host: is empty");
    expect(
      (await rejection(backend.put_bucket_logging({ ...ref, target: { target_bucket: "bad/target", target_prefix: "" } }))).code,
    ).toBe("usage");
    expect((await rejection(backend.get_bucket_policy({ ...ref, bucket: "Not A Bucket" }))).code).toBe("usage");
    // S3 offers acceleration for names without dots only.
    await backend.create_bucket({ account_id: id, name: "my.dotted.bucket", region: null });
    const dotted = await rejection(backend.put_bucket_acceleration({ account_id: id, bucket: "my.dotted.bucket", status: "enabled" }));
    expect(dotted.code).toBe("usage");
    expect(dotted.message).toContain("without dots");
    expect(await backend.validate_bucket_policy({ policy: POLICY })).toEqual([]);
    expect((await backend.validate_bucket_policy({ policy: "{" }))[0]?.message).toMatch(/^not valid JSON/);
    expect((await rejection(backend.format_bucket_policy({ policy: "{" }))).code).toBe("usage");
    expect(await backend.format_bucket_policy({ policy: '{"a":[]}' })).toBe('{\n  "a": []\n}');
  });

  it("reports each section on its own: failed, unsupported, loaded", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    backend.failBucketSection(id, "demo-photos", "policy", { code: "auth", message: "Access Denied (AccessDenied)" });
    const config = await backend.get_bucket_config({ account_id: id, bucket: "demo-photos" });
    expect(config.policy).toEqual({
      type: "failed",
      error: { code: "auth", message: "reading the policy of bucket 'demo-photos': Access Denied (AccessDenied)" },
    });
    expect(config.public_access_block.type).toBe("loaded");
    expect(config.versioning.type).toBe("loaded");
    expect((await rejection(backend.get_bucket_policy({ account_id: id, bucket: "demo-photos" }))).code).toBe("auth");

    // A custom endpoint implements policy and versioning only, and never acceleration.
    const archive = await accountId(backend, "Archive (custom endpoint)");
    const custom = await backend.get_bucket_config({ account_id: archive, bucket: "archive-2025" });
    expect(custom.policy.type).toBe("loaded");
    expect(custom.versioning).toEqual({ type: "loaded", value: { status: "enabled", mfa_delete: false } });
    for (const section of [custom.public_access_block, custom.logging, custom.website]) {
      expect(section.type).toBe("unsupported");
    }
    expect(custom.acceleration).toEqual({
      type: "unsupported",
      message: "Transfer acceleration is available on AWS S3 only; this account uses a custom endpoint.",
    });
    expect((await rejection(backend.get_bucket_acceleration({ account_id: archive, bucket: "archive-2025" }))).code).toBe("usage");

    // A bucket that does not exist fails every section as not found.
    const missing = await backend.get_bucket_config({ account_id: id, bucket: "no-such-bucket" });
    for (const section of [missing.policy, missing.public_access_block, missing.versioning, missing.logging, missing.website, missing.acceleration]) {
      expect(section.type).toBe("failed");
      expect(section.type === "failed" ? section.error.code : "").toBe("not_found");
    }
  });

  it("pins external buckets, merges them into the list and removes them again", async () => {
    const backend = create();
    const id = await accountId(backend, "Demo");
    const names = async () => (await backend.list_buckets({ account_id: id })).map((bucket) => bucket.name);

    // The bucket belongs to another account: it is not listed, and not usable until it is shared.
    expect(await names()).not.toContain("partner-exchange");
    expect(await backend.list_external_buckets({ account_id: id })).toEqual([]);
    expect((await rejection(backend.add_external_bucket({ account_id: id, name: "no-such-bucket" }))).code).toBe("not_found");
    expect((await rejection(backend.add_external_bucket({ account_id: id, name: "bad name" }))).code).toBe("usage");

    await backend.add_external_bucket({ account_id: id, name: "partner-exchange" });
    await backend.add_external_bucket({ account_id: id, name: "partner-exchange" });
    expect(await backend.list_external_buckets({ account_id: id })).toEqual(["partner-exchange"]);
    const listed = await backend.list_buckets({ account_id: id });
    expect(listed.find((bucket) => bucket.name === "partner-exchange")).toEqual({
      name: "partner-exchange",
      region: null,
      creation_date: null,
      owner: null,
      kind: "external",
    });
    // A pinned bucket that is also listed keeps its listed entry.
    await backend.add_external_bucket({ account_id: id, name: "demo-photos" });
    expect((await backend.list_buckets({ account_id: id })).filter((bucket) => bucket.name === "demo-photos")).toHaveLength(1);
    expect((await names()).slice().sort()).toEqual(await names());
    // The shared bucket can be used.
    expect((await backend.list_objects({ account_id: id, bucket: "partner-exchange", prefix: "", flat: false, token: null })).entries.length).toBeGreaterThan(0);

    // When listing is refused, the pinned buckets are all there is; without pins it is the error.
    backend.denyListing(id);
    expect((await names()).sort()).toEqual(["demo-photos", "partner-exchange"]);
    await backend.remove_external_bucket({ account_id: id, name: "partner-exchange" });
    await backend.remove_external_bucket({ account_id: id, name: "demo-photos" });
    expect((await rejection(backend.list_buckets({ account_id: id }))).code).toBe("auth");
    expect((await rejection(backend.remove_external_bucket({ account_id: id, name: "partner-exchange" }))).code).toBe("not_found");

    // Another account has its own pins.
    const archive = await accountId(backend, "Archive (custom endpoint)");
    expect(await backend.list_external_buckets({ account_id: archive })).toEqual([]);
  });
});
