// Edit in place for the mock backend (docs/desktop-api.md, "Edit in place (v0.2.0)"): sessions over
// objects of the mock world, with the same conflict rules as the real backend. There is no file watcher
// here: tests and demos call `save` to play the part of the user's editor saving the local copy, and
// `changeRemote` or `deleteRemote` to play somebody else changing the object.

import { BackendError } from "../api/errors";
import type { BbErrorCode, EditConflict, EditResolution, EditResolved, EditSession } from "../api/types";
import { copyKey } from "../lib/edit";
import { isFolderKey } from "../lib/s3keys";
import { nowIso, seededBytes, toHex, uuid } from "./util";
import type { MockBucket } from "./world";

export interface EditHost {
  /** The bucket as the account sees it (credentials and ownership rules apply); throws. */
  bucket(accountId: string, name: string): MockBucket;
  log(message: string): void;
  /** Delivers `edit://changed`. */
  emit(session: EditSession): void;
}

interface Record {
  session: EditSession;
  /** The local copy has a save that was not uploaded. */
  pending: boolean;
}

function fail(code: BbErrorCode, message: string): never {
  throw new BackendError(code, message);
}

export class EditMock {
  private readonly host: EditHost;
  private readonly records = new Map<string, Record>();
  private counter = 0;

  constructor(host: EditHost) {
    this.host = host;
  }

  list(): EditSession[] {
    return [...this.records.values()]
      .map((record) => ({ ...record.session }))
      .sort((a, b) => a.started_at.localeCompare(b.started_at) || a.id.localeCompare(b.id));
  }

  start(accountId: string, bucket: string, key: string): { session: EditSession; reused: boolean } {
    if (key === "" || isFolderKey(key)) {
      fail("usage", `'${key}' is a folder; only objects can be edited.`);
    }
    const target = this.host.bucket(accountId, bucket);
    for (const record of this.records.values()) {
      const { session } = record;
      if (session.account_id === accountId && session.bucket === bucket && session.key === key) {
        return { session: { ...session }, reused: true };
      }
    }
    const object = target.objects.get(key);
    if (object === undefined) {
      fail("not_found", "The object does not exist (NoSuchKey).");
    }
    this.counter += 1;
    const id = uuid();
    const name = key.slice(key.lastIndexOf("/") + 1);
    const session: EditSession = {
      id,
      account_id: accountId,
      bucket,
      key,
      local_path: `/home/demo/.local/share/bucketbridge/edit/${id}/${name}`,
      state: "watching",
      etag: object.etag,
      version_id: null,
      started_at: nowIso(),
      last_upload_at: null,
      uploads: 0,
      error: null,
      conflict: null,
    };
    this.records.set(id, { session, pending: false });
    this.host.log(`Editing s3://${bucket}/${key} from ${session.local_path}.`);
    this.publish(session);
    return { session: { ...session }, reused: false };
  }

  stop(id: string, discard: boolean): void {
    const record = this.record(id);
    if (!discard) {
      if (record.session.state === "conflict") {
        fail(
          "conflict",
          `s3://${record.session.bucket}/${record.session.key} changed on the server since it was opened; overwrite it, save a copy or discard your changes first.`,
        );
      }
      if (record.pending) {
        this.upload(record, false);
        if (this.stateOf(record) === "conflict") {
          fail(
            "conflict",
            `s3://${record.session.bucket}/${record.session.key} changed on the server since it was opened; overwrite it, save a copy or discard your changes first.`,
          );
        }
        if (this.stateOf(record) === "error") {
          fail("remote", record.session.error ?? "The upload failed.");
        }
      }
    }
    this.end(record);
  }

  resolve(id: string, resolution: EditResolution): EditResolved {
    const record = this.record(id);
    const { session } = record;
    const needs = resolution === "retry" ? "error" : "conflict";
    if (resolution === "discard" ? session.state !== "conflict" && session.state !== "error" : session.state !== needs) {
      fail("usage", `The session is not in the ${resolution === "discard" ? "conflict or error" : needs} state.`);
    }
    switch (resolution) {
      case "discard":
        this.end(record);
        return { session: null, copy_key: null };
      case "save_copy": {
        const target = this.host.bucket(session.account_id, session.bucket);
        const source = target.objects.get(session.key);
        let n = 1;
        while (target.objects.has(copyKey(session.key, n))) {
          n += 1;
        }
        const key = copyKey(session.key, n);
        target.objects.set(key, {
          ...(source ?? { size: 0, storage_class: "STANDARD", checksums: emptyChecksums() }),
          size: (source?.size ?? 0) + 1,
          etag: this.newEtag(),
          last_modified: nowIso(),
        });
        this.host.log(`Saved your version of s3://${session.bucket}/${session.key} as s3://${session.bucket}/${key}.`);
        this.end(record);
        return { session: null, copy_key: key };
      }
      case "overwrite":
      case "retry":
        this.upload(record, resolution === "overwrite");
        if (this.stateOf(record) === "conflict") {
          fail("conflict", "The object changed again; resolve the conflict again.");
        }
        return { session: { ...record.session }, copy_key: null };
    }
  }

  // ---------------------------------------------------------------- test and demo helpers

  /** The user saves the local copy: it is uploaded unless the object changed meanwhile. */
  save(id: string): void {
    const record = this.record(id);
    record.pending = true;
    if (record.session.state !== "conflict") {
      this.upload(record, false);
    }
  }

  /** Somebody else replaces the object on the server. */
  changeRemote(accountId: string, bucket: string, key: string): void {
    const object = this.host.bucket(accountId, bucket).objects.get(key);
    if (object !== undefined) {
      object.etag = this.newEtag();
      object.last_modified = nowIso();
    }
  }

  /** Somebody else deletes the object. */
  deleteRemote(accountId: string, bucket: string, key: string): void {
    this.host.bucket(accountId, bucket).objects.delete(key);
  }

  // ---------------------------------------------------------------- internals

  private record(id: string): Record {
    const record = this.records.get(id);
    if (record === undefined) {
      fail("not_found", `edit session ${id}`);
    }
    return record;
  }

  /** The state now (a call may have changed it, which the compiler cannot see). */
  private stateOf(record: Record): EditSession["state"] {
    return record.session.state;
  }

  private newEtag(): string {
    this.counter += 1;
    return toHex(seededBytes(`edit-etag:${this.counter}`, 16));
  }

  /** The upload of a save: checks the object first, unless the user chose to overwrite. */
  private upload(record: Record, overwrite: boolean): void {
    const { session } = record;
    this.update(record, { state: "uploading", error: null });
    const object = this.host.bucket(session.account_id, session.bucket).objects.get(session.key);
    if (!overwrite) {
      const conflict = this.conflictOf(session, object?.etag ?? null, object?.last_modified ?? null, object === undefined);
      if (conflict !== null) {
        this.update(record, { state: "conflict", conflict });
        return;
      }
    }
    const target = this.host.bucket(session.account_id, session.bucket);
    const etag = this.newEtag();
    target.objects.set(session.key, {
      ...(object ?? { size: 0, storage_class: "STANDARD", checksums: emptyChecksums() }),
      size: (object?.size ?? 0) + 1,
      etag,
      last_modified: nowIso(),
    });
    record.pending = false;
    this.update(record, {
      state: "uploaded",
      etag,
      uploads: session.uploads + 1,
      last_upload_at: nowIso(),
      error: null,
      conflict: null,
    });
    this.host.log(`Uploaded the saved copy of s3://${session.bucket}/${session.key}.`);
  }

  private conflictOf(session: EditSession, etag: string | null, modified: string | null, deleted: boolean): EditConflict | null {
    if (deleted) {
      return { remote_deleted: true, remote_etag: null, remote_modified: null };
    }
    return etag !== session.etag ? { remote_deleted: false, remote_etag: etag, remote_modified: modified } : null;
  }

  private update(record: Record, change: Partial<EditSession>): void {
    record.session = { ...record.session, ...change };
    this.publish(record.session);
  }

  private end(record: Record): void {
    this.records.delete(record.session.id);
    this.publish({ ...record.session, state: "stopped", error: null, conflict: null });
  }

  private publish(session: EditSession): void {
    this.host.emit({ ...session });
  }
}

function emptyChecksums() {
  return { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null };
}
