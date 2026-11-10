import { afterEach, describe, expect, it } from "vitest";
import { BackendError } from "../api/errors";
import { MockBackend } from "./mockBackend";

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

async function reference(backend: MockBackend): Promise<{ account_id: string; bucket: string }> {
  const id = (await backend.list_accounts()).default_account_id;
  if (id === null) {
    throw new Error("no default account");
  }
  return { account_id: id, bucket: "demo-empty" };
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

describe("mock bucket encryption", () => {
  it("round trips and treats nothing configured as an answer", async () => {
    const backend = create();
    const ref = await reference(backend);
    expect(await backend.get_bucket_encryption(ref)).toBeNull();
    const config = { algorithm: "aws:kms", kms_key_id: "alias/k", bucket_key_enabled: true } as const;
    await backend.put_bucket_encryption({ ...ref, config });
    expect(await backend.get_bucket_encryption(ref)).toEqual(config);
    await backend.delete_bucket_encryption(ref);
    await backend.delete_bucket_encryption(ref);
    expect(await backend.get_bucket_encryption(ref)).toBeNull();
  });

  it("rejects what the core rejects", async () => {
    const backend = create();
    const ref = await reference(backend);
    const bad = [
      { algorithm: "AES256", kms_key_id: "alias/k", bucket_key_enabled: false },
      { algorithm: "AES256", kms_key_id: null, bucket_key_enabled: true },
      { algorithm: "aws:kms", kms_key_id: "a b", bucket_key_enabled: false },
      { algorithm: "aws:kms:dsse", kms_key_id: null, bucket_key_enabled: false },
    ] as const;
    for (const config of bad) {
      expect((await rejection(backend.put_bucket_encryption({ ...ref, config }))).code).toBe("usage");
    }
    expect((await rejection(backend.get_bucket_encryption({ ...ref, bucket: "no-such-bucket" }))).code).toBe("not_found");
  });
});

describe("mock create_bucket options", () => {
  const block = { block_public_acls: true, ignore_public_acls: true, block_public_policy: true, restrict_public_buckets: true };

  it("creates the bucket with Object Lock, versioning, ownership and the public access block", async () => {
    const backend = create();
    const ref = await reference(backend);
    const created = { account_id: ref.account_id, bucket: "new-locked" };
    const result = await backend.create_bucket({
      account_id: ref.account_id,
      name: "new-locked",
      region: null,
      options: { object_lock: true, ownership: "BucketOwnerEnforced", public_access_block: block },
    });
    expect(result).toEqual({ warnings: [] });
    expect((await backend.get_bucket_object_lock(created)).enabled).toBe(true);
    expect((await backend.get_bucket_versioning(created)).status).toBe("enabled");
    expect(await backend.get_object_ownership(created)).toBe("BucketOwnerEnforced");
    expect(await backend.get_public_access_block(created)).toEqual(block);
    expect((await backend.get_bucket_acl(created)).acls_disabled).toBe(true);
  });

  it("leaves everything to the server without options and forgets a deleted bucket", async () => {
    const backend = create();
    const ref = await reference(backend);
    const created = { account_id: ref.account_id, bucket: "new-plain" };
    expect(await backend.create_bucket({ account_id: ref.account_id, name: "new-plain", region: null })).toEqual({ warnings: [] });
    expect(await backend.get_object_ownership(created)).toBeNull();
    expect((await backend.get_bucket_object_lock(created)).enabled).toBe(false);
    await backend.create_bucket({
      account_id: ref.account_id,
      name: "new-again",
      region: null,
      options: { object_lock: true, ownership: "ObjectWriter", public_access_block: null },
    });
    await backend.delete_bucket({ account_id: ref.account_id, name: "new-again" });
    await backend.create_bucket({ account_id: ref.account_id, name: "new-again", region: null });
    expect((await backend.get_bucket_object_lock({ account_id: ref.account_id, bucket: "new-again" })).enabled).toBe(false);
    expect(await backend.get_object_ownership({ account_id: ref.account_id, bucket: "new-again" })).toBeNull();
  });

  it("warns when a custom endpoint has no public access block", async () => {
    const backend = create();
    const archive = (await backend.list_accounts()).accounts.find((account) => account.label === "Archive (custom endpoint)");
    if (archive === undefined) {
      throw new Error("no archive account");
    }
    const result = await backend.create_bucket({
      account_id: archive.id,
      name: "archive-new",
      region: null,
      options: { object_lock: false, ownership: null, public_access_block: block },
    });
    expect(result.warnings).toHaveLength(1);
    expect(result.warnings[0]).toContain("public access block");
  });
});

describe("mock bucket tags", () => {
  it("replaces the whole set and an empty set removes it", async () => {
    const backend = create();
    const ref = await reference(backend);
    expect(await backend.get_bucket_tags(ref)).toEqual({});
    await backend.put_bucket_tags({ ...ref, tags: { team: "data", env: "prod" } });
    expect(await backend.get_bucket_tags(ref)).toEqual({ team: "data", env: "prod" });
    await backend.put_bucket_tags({ ...ref, tags: { team: "ops" } });
    expect(await backend.get_bucket_tags(ref)).toEqual({ team: "ops" });
    await backend.put_bucket_tags({ ...ref, tags: {} });
    expect(await backend.get_bucket_tags(ref)).toEqual({});
  });

  it("applies the S3 limits for buckets", async () => {
    const backend = create();
    const ref = await reference(backend);
    const fifty = Object.fromEntries(Array.from({ length: 50 }, (_, n) => [`k${n}`, "v"]));
    await backend.put_bucket_tags({ ...ref, tags: fifty });
    const fiftyOne = { ...fifty, extra: "v" };
    expect((await rejection(backend.put_bucket_tags({ ...ref, tags: fiftyOne }))).code).toBe("usage");
    const invalid: Record<string, string>[] = [{ "aws:x": "v" }, { "": "v" }, { k: "bad#" }, { k: "v".repeat(257) }];
    for (const tags of invalid) {
      expect((await rejection(backend.put_bucket_tags({ ...ref, tags }))).code).toBe("usage");
    }
    expect(Object.keys(await backend.get_bucket_tags(ref))).toHaveLength(50);
  });

  it("keeps the ACL of a bucket as grants and names the canned ACL they equal", async () => {
    const backend = create();
    const ref = await reference(backend);
    const start = await backend.get_bucket_acl(ref);
    expect(start.acl.canned).toBe("private");
    expect(start.acls_disabled).toBe(false);
    await backend.set_bucket_acl({ ...ref, acl: "public-read" });
    const info = await backend.get_bucket_acl(ref);
    expect(info.acl.canned).toBe("public-read");
    expect(info.acl.grants).toContainEqual({
      grantee_type: "group",
      grantee: "http://acs.amazonaws.com/groups/global/AllUsers",
      permission: "READ",
    });
    const custom = [
      { grantee_type: "canonical_user", grantee: "demo-owner-canonical-id", permission: "FULL_CONTROL" },
      { grantee_type: "email", grantee: "dev@example.com", permission: "READ" },
    ] as const;
    await backend.put_bucket_acl_grants({ ...ref, grants: [...custom] });
    expect((await backend.get_bucket_acl(ref)).acl).toEqual({ owner: "demo-owner-canonical-id", grants: [...custom], canned: null });
  });

  it("rejects bad ACL input, and refuses changes when ACLs are disabled", async () => {
    const backend = create();
    const ref = await reference(backend);
    expect((await rejection(backend.set_bucket_acl({ ...ref, acl: "bucket-owner-read" }))).code).toBe("usage");
    const bad = [
      [],
      [{ grantee_type: "group", grantee: "http://example.com/x", permission: "READ" }],
      [{ grantee_type: "email", grantee: "nobody", permission: "READ" }],
      [
        { grantee_type: "canonical_user", grantee: "a", permission: "READ" },
        { grantee_type: "canonical_user", grantee: "a", permission: "READ" },
      ],
    ] as const;
    for (const grants of bad) {
      expect((await rejection(backend.put_bucket_acl_grants({ ...ref, grants: [...grants] }))).code).toBe("usage");
    }
    await backend.put_object_ownership({ ...ref, ownership: "BucketOwnerEnforced" });
    expect((await backend.get_bucket_acl(ref)).acls_disabled).toBe(true);
    expect((await rejection(backend.set_bucket_acl({ ...ref, acl: "public-read" }))).code).toBe("conflict");
    const owner = [{ grantee_type: "canonical_user", grantee: "demo-owner-canonical-id", permission: "FULL_CONTROL" }] as const;
    expect((await rejection(backend.put_bucket_acl_grants({ ...ref, grants: [...owner] }))).code).toBe("conflict");
    await expect(
      backend.put_object_acl_grants({ ...ref, bucket: "demo-website", key: "index.html", grants: [...owner] }),
    ).rejects.toMatchObject({ code: "conflict" });
  });

  it("forgets the settings of a deleted bucket", async () => {
    const backend = create();
    const ref = await reference(backend);
    await backend.put_bucket_tags({ ...ref, tags: { a: "b" } });
    await backend.put_bucket_encryption({ ...ref, config: { algorithm: "AES256", kms_key_id: null, bucket_key_enabled: false } });
    await backend.delete_bucket({ account_id: ref.account_id, name: ref.bucket });
    await backend.create_bucket({ account_id: ref.account_id, name: ref.bucket, region: null });
    expect(await backend.get_bucket_tags(ref)).toEqual({});
    expect(await backend.get_bucket_encryption(ref)).toBeNull();
  });
});
