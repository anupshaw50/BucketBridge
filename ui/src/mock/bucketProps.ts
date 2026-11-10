/* eslint-disable @typescript-eslint/require-await -- the mock answers synchronously from memory */
// Default encryption and tags of the buckets of the mock cloud, behaving like the real commands
// (docs/desktop-api.md, "Bucket encryption, tags, ACLs and creation options"): nothing configured
// is an answer, and invalid input is rejected with the backend's `usage` errors.

import type { CommandArgs } from "../api/backend";
import { BackendError } from "../api/errors";
import type { AclGrant, BucketAcl, BucketEncryption, CannedAcl, ObjectOwnership } from "../api/types";
import { validateGrantList } from "../lib/aclGrants";
import { MAX_BUCKET_TAGS, validateKmsKeyId } from "../lib/bucketProps";
import { validateTag } from "../lib/objectMeta";
import { aclsDisabledMessage, cannedOf, grantsOf } from "./objectMeta";
import { clone } from "./util";

interface Where {
  account_id: string;
  bucket: string;
}

function fail(message: string): never {
  throw new BackendError("usage", message);
}

const OWNER = "demo-owner-canonical-id";

/** The rules of `BucketEncryption::validate` in the core. */
export function checkEncryption(config: BucketEncryption): void {
  switch (config.algorithm) {
    case "AES256":
      if (config.kms_key_id !== null) {
        fail("a KMS key can only be given with SSE-KMS");
      }
      if (config.bucket_key_enabled) {
        fail("a bucket key can only be used with SSE-KMS");
      }
      return;
    case "aws:kms": {
      const key = config.kms_key_id;
      if (key !== null && (key.trim().length === 0 || validateKmsKeyId(key) !== null)) {
        fail("the KMS key must be a key ID, key ARN, alias name or alias ARN without spaces");
      }
      return;
    }
    case "aws:kms:dsse":
      fail("dual-layer SSE-KMS cannot be set here; use SSE-S3 or SSE-KMS");
  }
}

/** The rules of `validate_bucket_tags` in the core. */
export function checkBucketTags(tags: Readonly<Record<string, string>>): void {
  const entries = Object.entries(tags);
  if (entries.length > MAX_BUCKET_TAGS) {
    fail(`${entries.length} tags are too many; a bucket can have at most ${MAX_BUCKET_TAGS}`);
  }
  for (const [key, value] of entries) {
    const problem = validateTag(key, value);
    if (problem !== null) {
      fail(problem);
    }
  }
}

export class MockBucketProps {
  private readonly encryption = new Map<string, BucketEncryption>();
  private readonly tags = new Map<string, Record<string, string>>();
  private readonly acls = new Map<string, AclGrant[]>();
  private readonly ownership: (args: Where) => ObjectOwnership | null;

  /** `ownership` tells the Object Ownership the mock cloud has for a bucket. */
  constructor(ownership: (args: Where) => ObjectOwnership | null) {
    this.ownership = ownership;
  }

  private static id(args: Where): string {
    return `${args.account_id}/${args.bucket}`;
  }

  /** Forgets everything of a deleted bucket. */
  forget(args: Where): void {
    const id = MockBucketProps.id(args);
    this.encryption.delete(id);
    this.tags.delete(id);
    this.acls.delete(id);
  }

  async get_bucket_encryption(args: CommandArgs["get_bucket_encryption"]): Promise<BucketEncryption | null> {
    const found = this.encryption.get(MockBucketProps.id(args));
    return found === undefined ? null : clone(found);
  }

  async put_bucket_encryption(args: CommandArgs["put_bucket_encryption"]): Promise<void> {
    checkEncryption(args.config);
    this.encryption.set(MockBucketProps.id(args), clone(args.config));
  }

  async delete_bucket_encryption(args: CommandArgs["delete_bucket_encryption"]): Promise<void> {
    this.encryption.delete(MockBucketProps.id(args));
  }

  async get_bucket_tags(args: CommandArgs["get_bucket_tags"]): Promise<Record<string, string>> {
    return clone(this.tags.get(MockBucketProps.id(args)) ?? {});
  }

  async put_bucket_tags(args: CommandArgs["put_bucket_tags"]): Promise<void> {
    checkBucketTags(args.tags);
    if (Object.keys(args.tags).length === 0) {
      this.tags.delete(MockBucketProps.id(args));
    } else {
      this.tags.set(MockBucketProps.id(args), clone(args.tags));
    }
  }

  private aclsDisabled(args: Where): boolean {
    return this.ownership(args) === "BucketOwnerEnforced";
  }

  async get_bucket_acl(args: CommandArgs["get_bucket_acl"]): Promise<BucketAcl> {
    const grants = clone(this.acls.get(MockBucketProps.id(args)) ?? grantsOf("private"));
    const ownership = this.ownership(args);
    return {
      acl: { owner: OWNER, grants, canned: cannedOf(grants) },
      ownership,
      acls_disabled: ownership === "BucketOwnerEnforced",
    };
  }

  async set_bucket_acl(args: CommandArgs["set_bucket_acl"]): Promise<void> {
    const acl: CannedAcl = args.acl;
    if (acl === "bucket-owner-read" || acl === "bucket-owner-full-control") {
      fail(`the canned ACL '${acl}' applies to objects, not to buckets; use private, public-read, public-read-write or authenticated-read`);
    }
    if (this.aclsDisabled(args)) {
      throw new BackendError("conflict", aclsDisabledMessage(args.bucket));
    }
    this.acls.set(MockBucketProps.id(args), grantsOf(acl));
  }

  async put_bucket_acl_grants(args: CommandArgs["put_bucket_acl_grants"]): Promise<void> {
    const problem = validateGrantList(args.grants);
    if (problem !== null) {
      fail(problem);
    }
    if (this.aclsDisabled(args)) {
      throw new BackendError("conflict", aclsDisabledMessage(args.bucket));
    }
    this.acls.set(MockBucketProps.id(args), clone(args.grants));
  }
}
