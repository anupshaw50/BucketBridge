// The bucket configuration of the mock cloud: policy, public access block,
// versioning, logging, website and acceleration of every bucket, behaving like
// the real commands (docs/desktop-api.md, "Bucket configuration (T03)"):
// "nothing configured" is an answer, a refused or unimplemented setting fails
// on its own, and invalid input is rejected with the backend's `usage` errors.

import { BackendError } from "../api/errors";
import type {
  AccelerationStatus,
  BbError,
  BucketConfig,
  BucketVersioning,
  LoggingTarget,
  PolicyIssue,
  PublicAccessBlock,
  Section,
  VersioningStatus,
  WebsiteConfig,
} from "../api/types";
import { checkPolicy, compactPolicy, formatPolicy, issueText } from "../lib/policy";
import { websiteProblems } from "../lib/website";
import { clone } from "./util";

export type SectionName = keyof BucketConfig;

/** Where a bucket lives: its provider, and whether that provider is a custom endpoint. */
export interface BucketSlot {
  /** Provider key plus bucket name; unique across the mock cloud. */
  readonly key: string;
  readonly bucket: string;
  /** S3-compatible servers implement policy and versioning only. */
  readonly custom: boolean;
}

interface Settings {
  policy: string | null; // compact JSON, as it is sent
  block: PublicAccessBlock | null;
  versioning: BucketVersioning;
  logging: LoggingTarget | null;
  website: WebsiteConfig | null;
  acceleration: AccelerationStatus;
}

export const ACCELERATION_UNSUPPORTED =
  "Transfer acceleration is available on AWS S3 only; this account uses a custom endpoint.";

/** The sections an S3-compatible server in the mock does not implement. */
const NOT_IMPLEMENTED_ON_CUSTOM: readonly SectionName[] = ["public_access_block", "logging", "website"];

const FEATURES: Record<SectionName, string> = {
  policy: "bucket policies",
  public_access_block: "the public access block",
  versioning: "versioning",
  logging: "server access logging",
  website: "static website hosting",
  acceleration: "transfer acceleration",
};

const WHAT: Record<SectionName, string> = {
  policy: "the policy",
  public_access_block: "the public access block",
  versioning: "the versioning state",
  logging: "the access logging",
  website: "the website configuration",
  acceleration: "transfer acceleration",
};

/** A server answer of "not implemented": the combined read reports it as `unsupported`. */
class NotImplementedError extends BackendError {}

function fail(code: BbError["code"], message: string): never {
  throw new BackendError(code, message);
}

function emptySettings(): Settings {
  return {
    policy: null,
    block: null,
    versioning: { status: "unversioned", mfa_delete: false },
    logging: null,
    website: null,
    acceleration: "unset",
  };
}

/** A bucket name as the backend accepts it for an existing bucket (the legacy rules). */
export function checkExistingBucketName(name: string): string | null {
  if (name.length === 0) {
    return "bucket name is empty";
  }
  if (name.length > 255) {
    return `bucket name is ${name.length} characters long; the maximum is 255`;
  }
  const bad = [...name].find((char) => !/[A-Za-z0-9._-]/.test(char));
  if (bad !== undefined) {
    return `bucket name '${name}' contains the character ${JSON.stringify(bad)}; only letters, digits, '.', '_' and '-' are allowed`;
  }
  if (!/^[A-Za-z0-9]/.test(name)) {
    return `bucket name '${name}' must start with a letter or digit`;
  }
  return null;
}

function publicReadPolicy(bucket: string): string {
  return JSON.stringify({
    Version: "2012-10-17",
    Statement: [
      {
        Sid: "PublicRead",
        Effect: "Allow",
        Principal: "*",
        Action: ["s3:GetObject"],
        Resource: [`arn:aws:s3:::${bucket}/*`],
      },
    ],
  });
}

function secureTransportPolicy(bucket: string): string {
  return JSON.stringify({
    Version: "2012-10-17",
    Statement: [
      {
        Sid: "DenyInsecureTransport",
        Effect: "Deny",
        Principal: "*",
        Action: "s3:*",
        Resource: [`arn:aws:s3:::${bucket}`, `arn:aws:s3:::${bucket}/*`],
        Condition: { Bool: { "aws:SecureTransport": "false" } },
      },
    ],
  });
}

export class MockBucketConfigs {
  private readonly stores = new Map<string, Settings>();
  private readonly denied = new Map<string, BbError>();

  /** Starts with a few buckets of the demo world configured, so every tab has something to show. */
  seedDemo(awsKey: (bucket: string) => string, customKey: (bucket: string) => string): void {
    const photos = this.settings(awsKey("demo-photos"));
    photos.block = {
      block_public_acls: true,
      ignore_public_acls: true,
      block_public_policy: true,
      restrict_public_buckets: true,
    };
    const backups = this.settings(awsKey("demo-backups"));
    backups.versioning = { status: "enabled", mfa_delete: false };
    backups.logging = { target_bucket: "demo-logs", target_prefix: "backups/" };
    const website = this.settings(awsKey("demo-website"));
    website.website = {
      type: "hosting",
      index_document: "index.html",
      error_document: "error.html",
      routing_rules: [
        {
          condition: { key_prefix_equals: "docs/", http_error_code_returned_equals: null },
          redirect: {
            protocol: null,
            host_name: null,
            replace_key_prefix_with: "documents/",
            replace_key_with: null,
            http_redirect_code: null,
          },
        },
      ],
    };
    website.block = {
      block_public_acls: false,
      ignore_public_acls: false,
      block_public_policy: false,
      restrict_public_buckets: false,
    };
    website.policy = publicReadPolicy("demo-website");
    const vault = this.settings(awsKey("secure-vault"));
    vault.versioning = { status: "enabled", mfa_delete: true };
    vault.policy = secureTransportPolicy("secure-vault");
    this.settings(customKey("archive-2025")).versioning = { status: "enabled", mfa_delete: false };
  }

  /** For tests and demos: makes a section of a bucket fail with this error, as a refused request would. */
  failSection(key: string, section: SectionName, error: BbError): void {
    this.denied.set(`${key}\u0000${section}`, error);
  }

  /** Forgets the configuration of a bucket that was deleted. */
  forget(key: string): void {
    this.stores.delete(key);
    for (const entry of [...this.denied.keys()]) {
      if (entry.startsWith(`${key}\u0000`)) {
        this.denied.delete(entry);
      }
    }
  }

  private settings(key: string): Settings {
    let found = this.stores.get(key);
    if (found === undefined) {
      found = emptySettings();
      this.stores.set(key, found);
    }
    return found;
  }

  /** The settings of a bucket for one section, or the error the server would answer with. */
  private open(slot: BucketSlot, section: SectionName, action: "reading" | "saving" | "removing" = "reading"): Settings {
    const refused = this.denied.get(`${slot.key}\u0000${section}`);
    if (refused !== undefined) {
      fail(
        refused.code,
        `${action} ${WHAT[section]} of bucket '${slot.bucket}': ${refused.message}`,
      );
    }
    if (slot.custom && NOT_IMPLEMENTED_ON_CUSTOM.includes(section)) {
      throw new NotImplementedError(
        "remote",
        `${action} ${WHAT[section]} of bucket '${slot.bucket}': A header you provided implies functionality that is not implemented (NotImplemented, HTTP 501)`,
      );
    }
    return this.settings(slot.key);
  }

  // ---- policy ----

  getPolicy(slot: BucketSlot): string | null {
    const { policy } = this.open(slot, "policy");
    if (policy === null) {
      return null;
    }
    const formatted = formatPolicy(policy);
    return "text" in formatted ? formatted.text : policy;
  }

  putPolicy(slot: BucketSlot, text: string): void {
    const compact = compactPolicy(text);
    if ("issues" in compact) {
      fail("usage", `invalid bucket policy: ${compact.issues.map(issueText).join("; ")}`);
    }
    this.open(slot, "policy", "saving").policy = compact.text;
  }

  deletePolicy(slot: BucketSlot): void {
    this.open(slot, "policy", "removing").policy = null;
  }

  // ---- public access block ----

  getBlock(slot: BucketSlot): PublicAccessBlock | null {
    const { block } = this.open(slot, "public_access_block");
    return block === null ? null : { ...block };
  }

  putBlock(slot: BucketSlot, block: PublicAccessBlock): void {
    this.open(slot, "public_access_block", "saving").block = { ...block };
  }

  deleteBlock(slot: BucketSlot): void {
    this.open(slot, "public_access_block", "removing").block = null;
  }

  // ---- versioning ----

  getVersioning(slot: BucketSlot): BucketVersioning {
    return { ...this.open(slot, "versioning").versioning };
  }

  putVersioning(slot: BucketSlot, status: VersioningStatus): void {
    if (status === "unversioned") {
      fail("usage", "versioning cannot be switched off once it was enabled; choose Enabled or Suspended");
    }
    this.open(slot, "versioning", "saving").versioning.status = status;
  }

  // ---- logging ----

  getLogging(slot: BucketSlot): LoggingTarget | null {
    const { logging } = this.open(slot, "logging");
    return logging === null ? null : { ...logging };
  }

  putLogging(slot: BucketSlot, target: LoggingTarget): void {
    const problem = checkExistingBucketName(target.target_bucket);
    if (problem !== null) {
      fail("usage", `the log target bucket: ${problem}`);
    }
    if (new TextEncoder().encode(target.target_prefix).length > 1024) {
      fail(
        "usage",
        `the log prefix is ${target.target_prefix.length} bytes long; at most 1024 bytes are allowed`,
      );
    }
    this.open(slot, "logging", "saving").logging = { ...target };
  }

  disableLogging(slot: BucketSlot): void {
    this.open(slot, "logging", "saving").logging = null;
  }

  // ---- website ----

  getWebsite(slot: BucketSlot): WebsiteConfig | null {
    const { website } = this.open(slot, "website");
    return website === null ? null : clone(website);
  }

  putWebsite(slot: BucketSlot, website: WebsiteConfig): void {
    const problems = websiteProblems(website);
    if (problems.length > 0) {
      fail("usage", `invalid website configuration: ${problems.join("; ")}`);
    }
    this.open(slot, "website", "saving").website = clone(website);
  }

  deleteWebsite(slot: BucketSlot): void {
    this.open(slot, "website", "removing").website = null;
  }

  // ---- acceleration ----

  getAcceleration(slot: BucketSlot): AccelerationStatus {
    if (slot.custom) {
      fail("usage", ACCELERATION_UNSUPPORTED);
    }
    return this.open(slot, "acceleration").acceleration;
  }

  putAcceleration(slot: BucketSlot, status: AccelerationStatus): void {
    if (slot.custom) {
      fail("usage", ACCELERATION_UNSUPPORTED);
    }
    if (status === "unset") {
      fail(
        "usage",
        "transfer acceleration cannot be unset once it was configured; choose Enabled or Suspended",
      );
    }
    if (status === "enabled" && slot.bucket.includes(".")) {
      fail("usage", `transfer acceleration needs a bucket name without dots; '${slot.bucket}' has some`);
    }
    this.open(slot, "acceleration", "saving").acceleration = status;
  }

  // ---- everything at once ----

  /** Every section on its own: one that fails or is not implemented never hides the others. */
  config(slot: BucketSlot): BucketConfig {
    const read = <T>(section: SectionName, get: () => T): Section<T> => {
      try {
        return { type: "loaded", value: get() };
      } catch (error) {
        if (error instanceof NotImplementedError) {
          return {
            type: "unsupported",
            message: `This server does not support ${FEATURES[section]} (NotImplemented, HTTP 501).`,
          };
        }
        if (error instanceof BackendError) {
          return { type: "failed", error: error.toJSON() };
        }
        throw error;
      }
    };
    return {
      policy: read("policy", () => this.getPolicy(slot)),
      public_access_block: read("public_access_block", () => this.getBlock(slot)),
      versioning: read("versioning", () => this.getVersioning(slot)),
      logging: read("logging", () => this.getLogging(slot)),
      website: read("website", () => this.getWebsite(slot)),
      acceleration: slot.custom
        ? { type: "unsupported", message: ACCELERATION_UNSUPPORTED }
        : read("acceleration", () => this.getAcceleration(slot)),
    };
  }

  /**
   * What the combined read answers for a bucket that is missing or refused: every section fails on its own
   * with that error, except transfer acceleration of a custom endpoint, which is not supported at all.
   */
  static failedConfig(error: BbError, custom: boolean): BucketConfig {
    const failed: Section<never> = { type: "failed", error };
    return {
      policy: failed,
      public_access_block: failed,
      versioning: failed,
      logging: failed,
      website: failed,
      acceleration: custom ? { type: "unsupported", message: ACCELERATION_UNSUPPORTED } : failed,
    };
  }

  /** The issues of a policy text, as `validate_bucket_policy` answers. */
  static validate(text: string): PolicyIssue[] {
    return checkPolicy(text);
  }
}
