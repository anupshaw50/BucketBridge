import { BackendError } from "../api/errors";
import type { CommandArgs } from "../api/backend";
import type {
  AclGrant,
  BbErrorCode,
  CannedAcl,
  EncryptionChange,
  HeaderChange,
  HeaderChanges,
  ObjectAcl,
  ObjectChange,
  ObjectDetails,
  RestoreStatus,
  Sse,
  TagEdit,
} from "../api/types";
import {
  isArchiveClass,
  isCannedAcl,
  isStorageClass,
  MAX_TAGS,
  MAX_USER_META_BYTES,
  metaBytes,
  normalizeCustomerKey,
  STORAGE_CLASSES,
  validateCustomerKey,
  validateHeaderValue,
  validateMediaType,
  validateMetaKey,
  validateTag,
} from "../lib/objectMeta";
import { isFolderKey } from "../lib/s3keys";
import { nowIso, seededBytes, toBase64 } from "./util";
import type { MockBucket, MockObject, Provider } from "./world";

/** Metadata of a mock object. Never changed in place: edits replace it, so copies can share it. */
export interface MockObjectMeta {
  content_type: string | null;
  cache_control: string | null;
  content_disposition: string | null;
  content_encoding: string | null;
  website_redirect?: string | null;
  user_meta: Record<string, string>;
  tags: Record<string, string>;
  sse: Sse;
  acl: CannedAcl;
  restore: RestoreStatus | null;
}

/** What the metadata mock needs from the backend that hosts it. */
export interface MetaHost {
  /** Lock, MFA and credential checks, then the bucket (like every S3 call). */
  bucket(accountId: string, name: string): MockBucket;
  latency(): Promise<void>;
  log(message: string): void;
}

const MAX_BATCH_KEYS = 10_000;
const OWNER = "demo-owner-canonical-id";
const BUCKET_OWNER = "bucket-owner-canonical-id";
const ALL_USERS = "http://acs.amazonaws.com/groups/global/AllUsers";
const AUTHENTICATED_USERS = "http://acs.amazonaws.com/groups/global/AuthenticatedUsers";

/** Buckets that reject ACL changes, like buckets with Object Ownership BucketOwnerEnforced. */
const ACLS_DISABLED = new Set(["demo-website"]);

function fail(code: BbErrorCode, message: string): never {
  throw new BackendError(code, message);
}

const TYPES: Record<string, string> = {
  txt: "text/plain",
  md: "text/markdown",
  log: "text/plain",
  html: "text/html",
  css: "text/css",
  js: "text/javascript",
  svg: "image/svg+xml",
  jpg: "image/jpeg",
  png: "image/png",
  pdf: "application/pdf",
  gz: "application/gzip",
  tar: "application/x-tar",
  mp4: "video/mp4",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
};

function defaultMeta(key: string): MockObjectMeta {
  const extension = key.includes(".") ? (key.split(".").pop() ?? "") : "";
  return {
    content_type: TYPES[extension.toLowerCase()] ?? "binary/octet-stream",
    cache_control: null,
    content_disposition: null,
    content_encoding: null,
    user_meta: {},
    tags: {},
    sse: { type: "s3" },
    acl: "private",
    restore: null,
  };
}

function metaOf(object: MockObject, key: string): MockObjectMeta {
  return object.meta ?? defaultMeta(key);
}

/** The fingerprint S3 would report for an SSE-C key (here: of the normalised hex key). */
function fingerprint(hex: string): string {
  return toBase64(seededBytes(`sse-c:${hex}`, 16));
}

function grantsOf(acl: CannedAcl): AclGrant[] {
  const owner: AclGrant = { grantee_type: "canonical_user", grantee: OWNER, permission: "FULL_CONTROL" };
  const group = (uri: string, permission: AclGrant["permission"]): AclGrant => ({
    grantee_type: "group",
    grantee: uri,
    permission,
  });
  switch (acl) {
    case "private":
      return [owner];
    case "public-read":
      return [owner, group(ALL_USERS, "READ")];
    case "public-read-write":
      return [owner, group(ALL_USERS, "READ"), group(ALL_USERS, "WRITE")];
    case "authenticated-read":
      return [owner, group(AUTHENTICATED_USERS, "READ")];
    case "bucket-owner-read":
      return [owner, { grantee_type: "canonical_user", grantee: BUCKET_OWNER, permission: "READ" }];
    case "bucket-owner-full-control":
      return [owner, { grantee_type: "canonical_user", grantee: BUCKET_OWNER, permission: "FULL_CONTROL" }];
  }
}

/** The bucket-owner ACLs are grants to another account, which no canned name identifies. */
function aclOf(acl: CannedAcl): ObjectAcl {
  const canned = acl === "bucket-owner-read" || acl === "bucket-owner-full-control" ? null : acl;
  return { owner: OWNER, grants: grantsOf(acl), canned };
}

function applyHeader(current: string | null, change: HeaderChange): string | null {
  switch (change.action) {
    case "keep":
      return current;
    case "set":
      return change.value;
    case "remove":
      return null;
  }
}

/** Object tags, storage class, encryption, headers and ACLs of the mock cloud. */
export class ObjectMetaMock {
  private readonly host: MetaHost;

  constructor(host: MetaHost) {
    this.host = host;
  }

  async get_object_details(args: CommandArgs["get_object_details"]): Promise<ObjectDetails> {
    await this.host.latency();
    const bucket = this.host.bucket(args.account_id, args.bucket);
    const supplied = this.parseKey(args.sse_c_key);
    const object = this.objectOf(bucket, args.key);
    const meta = metaOf(object, args.key);
    this.checkKey(bucket.name, args.key, meta, supplied);
    const acl = aclOf(meta.acl);
    return {
      key: args.key,
      size: object.size,
      etag: object.etag,
      last_modified: object.last_modified,
      storage_class: object.storage_class,
      version_id: null,
      checksums: { ...object.checksums },
      meta: {
        content_type: meta.content_type,
        cache_control: meta.cache_control,
        content_disposition: meta.content_disposition,
        content_encoding: meta.content_encoding,
        website_redirect: meta.website_redirect ?? null,
        user_meta: { ...meta.user_meta },
        tags: { ...meta.tags },
        acl: acl.canned ?? "custom",
        sse: meta.sse,
        object_lock: null,
      },
      restore: meta.restore,
      archive_status: null,
      acl,
      tags_error: null,
      acl_error: null,
    };
  }

  async update_object_tags(args: CommandArgs["update_object_tags"]): Promise<ObjectChange[]> {
    await this.host.latency();
    const bucket = this.host.bucket(args.account_id, args.bucket);
    const edit = args.edit;
    this.validateTagEdit(edit);
    const results = this.batch(args.keys, (key) => {
      const object = this.objectOf(bucket, key);
      const meta = metaOf(object, key);
      const wanted = this.wantedTags(edit, meta.tags);
      if (JSON.stringify(Object.entries(wanted).sort()) === JSON.stringify(Object.entries(meta.tags).sort())) {
        return this.result(key, false, null);
      }
      object.meta = { ...meta, tags: wanted };
      return this.result(key, true, null);
    });
    this.host.log(`Edited the tags of ${this.changedCount(results)} objects in bucket ${args.bucket}.`);
    return results;
  }

  async set_object_storage_class(args: CommandArgs["set_object_storage_class"]): Promise<ObjectChange[]> {
    await this.host.latency();
    const bucket = this.host.bucket(args.account_id, args.bucket);
    const wanted: string = args.storage_class;
    if (!isStorageClass(wanted)) {
      fail("usage", `"${wanted}" is not a storage class this tool can set; use one of ${STORAGE_CLASSES.map((c) => c.value).join(", ")}.`);
    }
    const supplied = this.parseKey(args.sse_c_key);
    const results = this.batch(args.keys, (key) => {
      const object = this.objectOf(bucket, key);
      const meta = metaOf(object, key);
      this.checkKey(bucket.name, key, meta, supplied);
      const current = object.storage_class;
      if (current === wanted) {
        return this.result(key, false, current);
      }
      this.requireReadable(bucket.name, key, object, meta, "storage class");
      object.storage_class = wanted;
      object.last_modified = nowIso();
      return this.result(key, true, current);
    });
    this.host.log(`Set the storage class ${wanted} on ${this.changedCount(results)} objects in bucket ${args.bucket}.`);
    return results;
  }

  async set_object_encryption(args: CommandArgs["set_object_encryption"]): Promise<ObjectChange[]> {
    await this.host.latency();
    const bucket = this.host.bucket(args.account_id, args.bucket);
    const target = this.targetSse(args.encryption);
    const supplied = this.parseKey(args.source_sse_c_key);
    const results = this.batch(args.keys, (key) => {
      const object = this.objectOf(bucket, key);
      const meta = metaOf(object, key);
      this.checkKey(bucket.name, key, meta, supplied);
      const previous = meta.sse.type;
      if (JSON.stringify(meta.sse) === JSON.stringify(target) && !(target.type === "kms" && target.key_id === null)) {
        return this.result(key, false, previous);
      }
      this.requireReadable(bucket.name, key, object, meta, "encryption");
      object.meta = { ...meta, sse: target };
      object.last_modified = nowIso();
      return this.result(key, true, previous);
    });
    this.host.log(`Changed the encryption of ${this.changedCount(results)} objects in bucket ${args.bucket}.`);
    return results;
  }

  async update_object_headers(args: CommandArgs["update_object_headers"]): Promise<ObjectChange[]> {
    await this.host.latency();
    const bucket = this.host.bucket(args.account_id, args.bucket);
    const changes = args.changes;
    this.validateHeaderChanges(changes);
    const supplied = this.parseKey(args.sse_c_key);
    const results = this.batch(args.keys, (key) => {
      const object = this.objectOf(bucket, key);
      const meta = metaOf(object, key);
      this.checkKey(bucket.name, key, meta, supplied);
      const userMeta = { ...meta.user_meta };
      for (const name of changes.user_meta_remove) {
        delete userMeta[name.toLowerCase()];
      }
      for (const [name, value] of Object.entries(changes.user_meta_set)) {
        userMeta[name.toLowerCase()] = value;
      }
      const size = metaBytes(userMeta);
      if (size > MAX_USER_META_BYTES) {
        fail("usage", `The user metadata would be ${size} bytes; S3 allows at most ${MAX_USER_META_BYTES} bytes for all keys and values together.`);
      }
      const next: MockObjectMeta = {
        ...meta,
        content_type: applyHeader(meta.content_type, changes.content_type),
        cache_control: applyHeader(meta.cache_control, changes.cache_control),
        content_disposition: applyHeader(meta.content_disposition, changes.content_disposition),
        content_encoding: applyHeader(meta.content_encoding, changes.content_encoding),
        website_redirect: applyHeader(meta.website_redirect ?? null, changes.website_redirect ?? { action: "keep" }),
        user_meta: userMeta,
      };
      if (JSON.stringify(next) === JSON.stringify(meta)) {
        return this.result(key, false, null);
      }
      this.requireReadable(bucket.name, key, object, meta, "headers");
      object.meta = next;
      object.last_modified = nowIso();
      return this.result(key, true, null);
    });
    this.host.log(`Edited the headers of ${this.changedCount(results)} objects in bucket ${args.bucket}.`);
    return results;
  }

  async set_object_acl(args: CommandArgs["set_object_acl"]): Promise<ObjectChange[]> {
    await this.host.latency();
    const bucket = this.host.bucket(args.account_id, args.bucket);
    const wanted: string = args.acl;
    if (!isCannedAcl(wanted)) {
      fail("usage", `"${wanted}" is not a canned ACL; use one of private, public-read, public-read-write, authenticated-read, bucket-owner-read, bucket-owner-full-control.`);
    }
    const results = this.batch(args.keys, (key) => {
      const object = this.objectOf(bucket, key);
      const meta = metaOf(object, key);
      const current = aclOf(meta.acl);
      const previous = current.canned ?? "custom";
      if (current.canned === wanted) {
        return this.result(key, false, previous);
      }
      if (ACLS_DISABLED.has(bucket.name)) {
        if (wanted === "private" && current.canned === "private") {
          return this.result(key, false, previous);
        }
        fail("conflict", `ACLs are disabled for bucket '${bucket.name}' (Object Ownership is BucketOwnerEnforced); change the bucket's Object Ownership setting to use ACLs`);
      }
      object.meta = { ...meta, acl: wanted };
      return this.result(key, true, previous);
    });
    this.host.log(`Set the ACL ${wanted} on ${this.changedCount(results)} objects in bucket ${args.bucket}.`);
    return results;
  }

  // ---- internals ----

  private result(key: string, changed: boolean, previous: string | null): ObjectChange {
    return { key, changed, previous, error: null, warnings: [] };
  }

  private changedCount(results: readonly ObjectChange[]): number {
    return results.filter((result) => result.changed).length;
  }

  /** One result per distinct key; a failure is that key's result and never stops the others. */
  private batch(keys: readonly string[], run: (key: string) => ObjectChange): ObjectChange[] {
    if (keys.length === 0) {
      fail("usage", "Select at least one object.");
    }
    if (keys.length > MAX_BATCH_KEYS) {
      fail("usage", `${keys.length} objects are too many for one request; at most ${MAX_BATCH_KEYS} are accepted.`);
    }
    const results: ObjectChange[] = [];
    for (const key of new Set(keys)) {
      try {
        results.push(run(key));
      } catch (error) {
        if (!(error instanceof BackendError)) {
          throw error;
        }
        results.push({ key, changed: false, previous: null, error: error.toJSON(), warnings: [] });
      }
    }
    return results;
  }

  private objectOf(bucket: MockBucket, key: string): MockObject {
    if (isFolderKey(key)) {
      fail("usage", `'${key}' is a folder; select the objects in it instead.`);
    }
    return bucket.objects.get(key) ?? fail("not_found", `s3://${bucket.name}/${key}: The specified key does not exist (NoSuchKey).`);
  }

  /** The hex form of a key given as hex or base64; `null` when none was given. */
  private parseKey(text: string | null): string | null {
    if (text === null) {
      return null;
    }
    const problem = validateCustomerKey(text);
    if (problem !== null) {
      fail("usage", problem);
    }
    return normalizeCustomerKey(text);
  }

  private checkKey(bucket: string, key: string, meta: MockObjectMeta, supplied: string | null): void {
    const uri = `s3://${bucket}/${key}`;
    if (meta.sse.type === "c") {
      if (supplied === null) {
        fail("remote", `S3 rejected the request; if the object is encrypted with a customer-provided key (SSE-C), its key is needed: ${uri}: Bad Request (HTTP 400)`);
      }
      if (fingerprint(supplied) !== meta.sse.key_md5) {
        fail("auth", `S3 refused the customer-provided key (SSE-C): it is not the key this object was encrypted with, or access is denied: ${uri}: Forbidden (HTTP 403)`);
      }
    } else if (supplied !== null) {
      fail("remote", `S3 rejected the customer-provided key (SSE-C): the object may not be encrypted with a customer key: ${uri}: Bad Request (HTTP 400)`);
    }
  }

  /** An archived object that is not restored cannot be copied. */
  private requireReadable(bucket: string, key: string, object: MockObject, meta: MockObjectMeta, what: string): void {
    if (isArchiveClass(object.storage_class) && !(meta.restore !== null && !meta.restore.in_progress)) {
      fail("conflict", `s3://${bucket}/${key} is archived (${object.storage_class}) and has not been restored, so its ${what} cannot be changed; restore it first`);
    }
  }

  private wantedTags(edit: TagEdit, current: Readonly<Record<string, string>>): Record<string, string> {
    const wanted = edit.mode === "replace" ? { ...edit.tags } : { ...current };
    if (edit.mode === "merge") {
      for (const name of edit.remove) {
        delete wanted[name];
      }
      Object.assign(wanted, edit.set);
    }
    if (Object.keys(wanted).length > MAX_TAGS) {
      fail("usage", `${Object.keys(wanted).length} tags are too many; an object can have at most ${MAX_TAGS}.`);
    }
    return wanted;
  }

  private validateTagEdit(edit: TagEdit): void {
    const pairs = Object.entries(edit.mode === "replace" ? edit.tags : edit.set);
    if (pairs.length > MAX_TAGS) {
      fail("usage", `${pairs.length} tags are too many; an object can have at most ${MAX_TAGS}.`);
    }
    for (const [key, value] of pairs) {
      const problem = validateTag(key, value);
      if (problem !== null) {
        fail("usage", problem);
      }
    }
    if (edit.mode === "merge") {
      for (const key of edit.remove) {
        const problem = validateTag(key, "");
        if (problem !== null) {
          fail("usage", problem);
        }
        if (key in edit.set) {
          fail("usage", `The tag "${key}" is both set and removed.`);
        }
      }
    }
  }

  private validateHeaderChanges(changes: HeaderChanges): void {
    const redirect = changes.website_redirect;
    if (redirect?.action === "set" && !/^(\/|https?:\/\/)/.test(redirect.value)) {
      fail("usage", "The website redirect location must start with / or with http:// or https://.");
    }
    for (const [name, change] of [
      ["Content-Type", changes.content_type],
      ["Cache-Control", changes.cache_control],
      ["Content-Disposition", changes.content_disposition],
      ["Content-Encoding", changes.content_encoding],
    ] as const) {
      if (change.action === "set") {
        const problem = validateHeaderValue(name, change.value) ?? (name === "Content-Type" ? validateMediaType(change.value) : null);
        if (problem !== null) {
          fail("usage", problem);
        }
      }
    }
    const seen = new Set<string>();
    for (const [key, value] of Object.entries(changes.user_meta_set)) {
      const problem = validateMetaKey(key) ?? validateHeaderValue(`The value of "${key}"`, value);
      if (problem !== null) {
        fail("usage", problem);
      }
      seen.add(key.toLowerCase());
    }
    for (const key of changes.user_meta_remove) {
      const problem = validateMetaKey(key);
      if (problem !== null) {
        fail("usage", problem);
      }
      if (seen.has(key.toLowerCase())) {
        fail("usage", `The user metadata "${key}" is both set and removed.`);
      }
    }
  }

  private targetSse(change: EncryptionChange): Sse {
    switch (change.type) {
      case "s3":
        return { type: "s3" };
      case "kms":
        if (change.key_id !== null && (change.key_id.trim().length === 0 || /\s/.test(change.key_id))) {
          fail("usage", "The KMS key must be a key ID, key ARN, alias name or alias ARN without spaces.");
        }
        return { type: "kms", key_id: change.key_id, bucket_key: change.bucket_key };
      case "c": {
        const hex = this.parseKey(change.key);
        return { type: "c", key_md5: fingerprint(hex ?? "") };
      }
    }
  }
}

/** Demo objects with tags, metadata, encryption and an ACL, so the properties editor has something to show. */
export function seedObjectMeta(providers: ReadonlyMap<string, Provider>): void {
  const put = (bucket: string, key: string, patch: Partial<MockObjectMeta>) => {
    for (const provider of providers.values()) {
      const object = provider.buckets.get(bucket)?.objects.get(key);
      if (object) {
        object.meta = { ...defaultMeta(key), ...patch };
      }
    }
  };
  put("demo-photos", "readme.md", {
    cache_control: "max-age=300",
    user_meta: { author: "demo" },
    tags: { team: "docs" },
  });
  put("demo-photos", "documents/report-q3.pdf", {
    content_disposition: 'attachment; filename="report-q3.pdf"',
    tags: { env: "prod", "cost-center": "42" },
    sse: { type: "kms", key_id: "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab", bucket_key: true },
  });
  put("demo-website", "index.html", { cache_control: "public, max-age=3600", acl: "public-read" });
  put("demo-backups", "archive/2019.tar", { restore: null });
  // Encrypted with a customer key; the demo key is 32 bytes of 0x11 (64 hex digits "11" repeated).
  put("secure-vault", "keys/README.txt", { sse: { type: "c", key_md5: fingerprint("11".repeat(32)) } });
}
