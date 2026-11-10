/* eslint-disable @typescript-eslint/require-await -- the mock answers synchronously from memory */
import type { CommandArgs } from "../api/backend";
import { BackendError } from "../api/errors";
import type {
  ArchiveState,
  BucketObjectLock,
  LockMode,
  ObjectChange,
  ObjectLockState,
  ObjectOwnership,
} from "../api/types";

/** The words S3 uses when a locked object refuses a change. */
export const LOCKED_MESSAGE = "Access Denied because object protected by object lock.";

const change = (key: string, changed: boolean, previous: string | null = null, error: ObjectChange["error"] = null): ObjectChange => ({
  key,
  changed,
  previous,
  error,
  warnings: [],
});

/** Object Lock, ownership and restore state of the mock cloud (in memory, per bucket and key). */
export class ObjectLockMock {
  private readonly buckets = new Map<string, BucketObjectLock>();
  private readonly ownership = new Map<string, ObjectOwnership>();
  private readonly objects = new Map<string, ObjectLockState>();
  private readonly restores = new Map<string, ArchiveState>();

  private static id(args: { account_id: string; bucket: string }, key = ""): string {
    return `${args.account_id}/${args.bucket}/${key}`;
  }

  async get_bucket_object_lock(args: CommandArgs["get_bucket_object_lock"]): Promise<BucketObjectLock> {
    return this.buckets.get(ObjectLockMock.id(args)) ?? { enabled: false, default_retention: null };
  }

  async put_bucket_object_lock(args: CommandArgs["put_bucket_object_lock"]): Promise<void> {
    const rule = args.default_retention;
    if (rule !== null && (rule.days === null) === (rule.years === null)) {
      throw new BackendError("usage", "give the default retention in days or in years, not both and not neither");
    }
    this.buckets.set(ObjectLockMock.id(args), { enabled: true, default_retention: rule });
  }

  async get_object_lock(args: CommandArgs["get_object_lock"]): Promise<ObjectLockState> {
    return this.objects.get(ObjectLockMock.id(args, args.key)) ?? { mode: null, retain_until: null, legal_hold: false };
  }

  async set_object_retention(args: CommandArgs["set_object_retention"]): Promise<ObjectChange[]> {
    if (args.change.action === "set" && Date.parse(args.change.retain_until) <= Date.now()) {
      throw new BackendError("usage", "the retain-until date must be in the future");
    }
    return args.keys.map((key) => {
      const id = ObjectLockMock.id(args, key);
      const state = this.objects.get(id) ?? { mode: null, retain_until: null, legal_hold: false };
      const locked =
        state.legal_hold || state.mode === "compliance" || (state.mode === "governance" && !args.bypass_governance);
      if (args.change.action === "clear" && locked) {
        return change(key, false, null, { code: "auth", message: `changing the retention of s3://${args.bucket}/${key}: ${LOCKED_MESSAGE}` });
      }
      const mode: LockMode | null = args.change.action === "set" ? args.change.mode : null;
      this.objects.set(id, {
        ...state,
        mode,
        retain_until: args.change.action === "set" ? args.change.retain_until : null,
      });
      return change(key, true);
    });
  }

  async set_object_legal_hold(args: CommandArgs["set_object_legal_hold"]): Promise<ObjectChange[]> {
    return args.keys.map((key) => {
      const id = ObjectLockMock.id(args, key);
      const state = this.objects.get(id) ?? { mode: null, retain_until: null, legal_hold: false };
      const previous = state.legal_hold ? "on" : "off";
      if (state.legal_hold === args.on) {
        return change(key, false, previous);
      }
      this.objects.set(id, { ...state, legal_hold: args.on });
      return change(key, true, previous);
    });
  }

  /** The Object Ownership of a bucket as the mock cloud has it now; `null` when none is set. */
  ownershipOf(accountId: string, bucket: string): ObjectOwnership | null {
    return this.ownership.get(ObjectLockMock.id({ account_id: accountId, bucket })) ?? null;
  }

  /** Turns Object Lock on for a new bucket (the lock a bucket is created with). */
  enableLock(args: { account_id: string; bucket: string }): void {
    this.buckets.set(ObjectLockMock.id(args), { enabled: true, default_retention: null });
  }

  /** Forgets the lock, ownership and object states of a bucket that no longer exists. */
  forget(args: { account_id: string; bucket: string }): void {
    const prefix = ObjectLockMock.id(args, "");
    for (const map of [this.buckets, this.ownership, this.objects, this.restores]) {
      for (const key of [...map.keys()]) {
        if (key.startsWith(prefix)) {
          map.delete(key);
        }
      }
    }
  }

  /** Sets the Object Ownership without a request (the starting state of the demo). */
  seedOwnership(args: { account_id: string; bucket: string }, ownership: ObjectOwnership): void {
    this.ownership.set(ObjectLockMock.id(args), ownership);
  }

  async get_object_ownership(args: CommandArgs["get_object_ownership"]): Promise<ObjectOwnership | null> {
    return this.ownership.get(ObjectLockMock.id(args)) ?? null;
  }

  async put_object_ownership(args: CommandArgs["put_object_ownership"]): Promise<void> {
    this.ownership.set(ObjectLockMock.id(args), args.ownership);
  }

  async restore_objects(args: CommandArgs["restore_objects"]): Promise<ObjectChange[]> {
    if (args.days !== null && (args.days < 1 || args.days > 3650)) {
      throw new BackendError("usage", "restore for 1 to 3650 days");
    }
    return args.keys.map((key) => {
      const id = ObjectLockMock.id(args, key);
      if (this.restores.get(id)?.restore?.in_progress === true) {
        return change(key, false, null, { code: "conflict", message: "Object restore is already in progress" });
      }
      this.restores.set(id, {
        storage_class: "GLACIER",
        archived: true,
        restore: { in_progress: true, expires: null },
        kind: "glacier",
      });
      return change(key, true);
    });
  }

  async get_restore_state(args: CommandArgs["get_restore_state"]): Promise<ArchiveState> {
    return (
      this.restores.get(ObjectLockMock.id(args, args.key)) ?? { storage_class: "STANDARD", archived: false, restore: null, kind: null }
    );
  }
}
