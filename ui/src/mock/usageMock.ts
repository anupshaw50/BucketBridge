// The bucket usage command of the mock backend (docs/desktop-api.md, "Bucket usage"): totals of the
// current objects, the older versions and the unfinished multipart uploads of a bucket or prefix.
// Progress is reported after every 1000 keys and after every upload; `cancel_bucket_usage` makes the
// run reject with `cancelled` at the next step.

import type { CommandArgs } from "../api/backend";
import { BackendError } from "../api/errors";
import type { BucketUsage, UploadRow, UsageProgressEvent } from "../api/types";
import { validateBucketName } from "../lib/validation";
import { seededBytes } from "./util";
import type { MockVersions } from "./versions";
import type { MockBucket } from "./world";

const PAGE = 1000;
const PART_BYTES = 5 * 1024 * 1024;
const MAX_JOB_ID = 64;

export interface UsageHost {
  /** The bucket as the account sees it; throws like a real call. */
  bucket(accountId: string, name: string): MockBucket;
  /** The unfinished multipart uploads of one bucket. */
  uploads(accountId: string, bucket: string): UploadRow[];
  readonly versions: MockVersions;
  emit(event: UsageProgressEvent): void;
}

/** Parts of an upload, and their sizes: deterministic, so tests and screenshots are stable. */
export function mockUploadParts(uploadId: string): { parts: number; bytes: number } {
  const parts = 1 + ((seededBytes(`parts:${uploadId}`, 1)[0] ?? 0) % 4);
  return { parts, bytes: parts * PART_BYTES };
}

function fail(code: "usage" | "conflict" | "cancelled", message: string): never {
  throw new BackendError(code, message);
}

/** Lets other tasks (a click on Cancel) run between two steps. */
function yieldTurn(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

export class UsageMock {
  private readonly jobs = new Map<string, { cancelled: boolean }>();

  private readonly host: UsageHost;

  constructor(host: UsageHost) {
    this.host = host;
  }

  cancel(jobId: string): void {
    const job = this.jobs.get(jobId);
    if (job !== undefined) {
      job.cancelled = true;
    }
  }

  async compute(args: CommandArgs["compute_bucket_usage"]): Promise<BucketUsage> {
    if (args.job_id.length === 0 || args.job_id.length > MAX_JOB_ID) {
      fail("usage", "The job id must be 1 to 64 characters long.");
    }
    if (this.jobs.has(args.job_id)) {
      fail("conflict", `Job ${args.job_id} is already running.`);
    }
    const problem = validateBucketName(args.bucket);
    if (problem !== null) {
      fail("usage", `${args.bucket}: ${problem}`);
    }
    const bucket = this.host.bucket(args.account_id, args.bucket);
    const job = { cancelled: false };
    this.jobs.set(args.job_id, job);
    try {
      const step = async (phase: UsageProgressEvent["phase"], usage: BucketUsage) => {
        this.host.emit({ job_id: args.job_id, phase, usage: structuredClone(usage) });
        await yieldTurn();
        if (job.cancelled) {
          fail("cancelled", "The operation was cancelled.");
        }
      };
      const usage: BucketUsage = {
        objects: 0,
        bytes: 0,
        noncurrent: args.versions ? { versions: 0, bytes: 0, delete_markers: 0 } : null,
        multipart: args.multipart ? { uploads: 0, parts: 0, bytes: 0 } : null,
      };
      if (args.versions) {
        const noncurrent = usage.noncurrent!;
        const keys = this.host.versions.keysOf(bucket, args.prefix);
        let seen = 0;
        for (const key of keys) {
          for (const [index, entry] of this.host.versions.entries(bucket, key).entries()) {
            if (entry.is_delete_marker) {
              noncurrent.delete_markers += 1;
            } else if (index === 0) {
              usage.objects += 1;
              usage.bytes += entry.size;
            } else {
              noncurrent.versions += 1;
              noncurrent.bytes += entry.size;
            }
          }
          seen += 1;
          if (seen % PAGE === 0) {
            await step("objects", usage);
          }
        }
      } else {
        let seen = 0;
        for (const [key, object] of bucket.objects) {
          if (!key.startsWith(args.prefix)) {
            continue;
          }
          usage.objects += 1;
          usage.bytes += object.size;
          seen += 1;
          if (seen % PAGE === 0) {
            await step("objects", usage);
          }
        }
      }
      await step("objects", usage);
      if (args.multipart) {
        const total = usage.multipart!;
        for (const upload of this.host.uploads(args.account_id, args.bucket)) {
          if (!upload.key.startsWith(args.prefix)) {
            continue;
          }
          const { parts, bytes } = mockUploadParts(upload.upload_id);
          total.uploads += 1;
          total.parts += parts;
          total.bytes += bytes;
          await step("multipart", usage);
        }
      }
      return structuredClone(usage);
    } finally {
      this.jobs.delete(args.job_id);
    }
  }
}
