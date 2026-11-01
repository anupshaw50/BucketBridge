import { BackendError } from "../api/errors";
import type { CommandArgs } from "../api/backend";
import type { BbErrorCode, CseCompression, ObjectCse, TransformOutcome } from "../api/types";
import { isFolderKey } from "../lib/s3keys";

function fail(code: BbErrorCode, message: string): never {
  throw new BackendError(code, message);
}

interface Entry {
  password: string;
  compression: CseCompression;
}

/** Client-side encryption state of the mock: which objects are encrypted, with which password. */
export class CseMock {
  private readonly entries = new Map<string, Entry>();

  private static id(args: { account_id: string; bucket: string; key: string }): string {
    return `${args.account_id}/${args.bucket}/${args.key}`;
  }

  private require(args: { account_id: string; bucket: string; key: string }): void {
    if (isFolderKey(args.key)) {
      fail("usage", `s3://${args.bucket}/${args.key} is a folder; client-side encryption works on objects`);
    }
  }

  private probeOf(entry: Entry | undefined): ObjectCse {
    return {
      encrypted: entry !== undefined,
      marked: entry !== undefined,
      version: entry === undefined ? null : 1,
      compression: entry?.compression ?? null,
      chunk_size: entry === undefined ? null : 1048576,
    };
  }

  private checked(args: { account_id: string; bucket: string; key: string }, password: string | null): Entry {
    const entry = this.entries.get(CseMock.id(args));
    if (entry === undefined) {
      fail("usage", `s3://${args.bucket}/${args.key} is not client-side encrypted`);
    }
    if (password === null || password === "") {
      fail("auth", "password not found: the data is encrypted and no password was given");
    }
    if (password !== entry.password) {
      fail("auth", "password not valid: it does not open the data");
    }
    return entry;
  }

  async cse_probe_object(args: CommandArgs["cse_probe_object"]): Promise<ObjectCse> {
    await Promise.resolve();
    this.require(args);
    return this.probeOf(this.entries.get(CseMock.id(args)));
  }

  async cse_check_password(args: CommandArgs["cse_check_password"]): Promise<ObjectCse> {
    await Promise.resolve();
    this.require(args);
    return this.probeOf(this.checked(args, args.password));
  }

  async cse_encrypt_object(args: CommandArgs["cse_encrypt_object"]): Promise<TransformOutcome> {
    await Promise.resolve();
    this.require(args);
    if (args.password === "") {
      fail("usage", "the password must not be empty");
    }
    if (this.entries.has(CseMock.id(args))) {
      fail("conflict", `s3://${args.bucket}/${args.key} is already encrypted; decrypt it first`);
    }
    this.entries.set(CseMock.id(args), { password: args.password, compression: args.compression ?? "none" });
    return { size_before: 0, size_after: 0, etag: null };
  }

  async cse_decrypt_object(args: CommandArgs["cse_decrypt_object"]): Promise<TransformOutcome> {
    await Promise.resolve();
    this.require(args);
    this.checked(args, args.password);
    this.entries.delete(CseMock.id(args));
    return { size_before: 0, size_after: 0, etag: null };
  }
}
