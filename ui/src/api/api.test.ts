import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Backend } from "./backend";
import { COMMAND_NAMES } from "./backend";
import { BackendError, errorText, hasCode, toBbError } from "./errors";
import { MAX_PROMPTS_PER_CALL, runGuarded, type SessionPrompts, withSessionGuard } from "./guard";
import { createTauriBackend } from "./tauriBackend";
import { tr } from "../i18n";
import { SAMPLE_ARGS } from "../test/sampleArgs";

type EventHandler = (event: { payload: unknown }) => void;

/** The sample arguments of a command, or undefined for commands without arguments. */
function argsFor(name: string): unknown {
  return (SAMPLE_ARGS as unknown as Record<string, unknown>)[name];
}

const invoke = vi.fn<(command: string, args?: unknown) => Promise<unknown>>();
const listen = vi.fn<(event: string, handler: EventHandler) => Promise<() => void>>();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => invoke(command, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: EventHandler) => listen(event, handler),
}));

describe("error normalisation", () => {
  it("keeps contract errors and their codes", () => {
    const error = toBbError({ code: "conflict", message: "bucket not empty" });
    expect(error).toBeInstanceOf(BackendError);
    expect(error.toJSON()).toEqual({ code: "conflict", message: "bucket not empty" });
    expect(toBbError(error)).toBe(error);
  });

  it("turns strings, Errors and junk into internal errors with a message", () => {
    expect(toBbError("command list_objects not found").toJSON()).toEqual({
      code: "internal",
      message: "command list_objects not found",
    });
    expect(toBbError('{"code":"auth","message":"denied"}').code).toBe("auth");
    expect(toBbError(new Error("boom")).toJSON()).toEqual({ code: "internal", message: "boom" });
    expect(toBbError({ code: "weird", message: "x" }).code).toBe("internal");
    expect(toBbError(42).code).toBe("internal");
    expect(toBbError(undefined).message.length).toBeGreaterThan(0);
    // The fallback text is written in the interface source, so it follows the language.
    expect(toBbError({ code: "locked", message: "" }).message).toBe(
      tr("Stored secrets are locked. Unlock them with the master password."),
    );
    expect(errorText({ code: "io", message: "disk full" })).toBe("disk full");
    expect(hasCode({ code: "mfa_required", message: "code" }, "mfa_required")).toBe(true);
  });
});

function prompts(answers: { unlock?: boolean[]; mfa?: boolean[] }): SessionPrompts & {
  unlockCalls: number;
  mfaCalls: string[];
} {
  const unlock = [...(answers.unlock ?? [])];
  const mfa = [...(answers.mfa ?? [])];
  const state = {
    unlockCalls: 0,
    mfaCalls: [] as string[],
    requestUnlock() {
      state.unlockCalls += 1;
      return Promise.resolve(unlock.shift() ?? false);
    },
    requestMfa(accountId: string) {
      state.mfaCalls.push(accountId);
      return Promise.resolve(mfa.shift() ?? false);
    },
  };
  return state;
}

describe("session guard", () => {
  it("unlocks and retries a locked call", async () => {
    const operation = vi
      .fn<() => Promise<string>>()
      .mockRejectedValueOnce({ code: "locked", message: "locked" })
      .mockResolvedValueOnce("ok");
    const asked = prompts({ unlock: [true] });
    await expect(runGuarded(operation, null, asked)).resolves.toBe("ok");
    expect(asked.unlockCalls).toBe(1);
    expect(operation).toHaveBeenCalledTimes(2);
  });

  it("asks for an MFA code with the account of the call", async () => {
    const operation = vi
      .fn<() => Promise<number>>()
      .mockRejectedValueOnce({ code: "mfa_required", message: "mfa" })
      .mockResolvedValueOnce(1);
    const asked = prompts({ mfa: [true] });
    await expect(runGuarded(operation, "acc-9", asked)).resolves.toBe(1);
    expect(asked.mfaCalls).toEqual(["acc-9"]);
  });

  it("rejects when the prompt is dismissed, or without an account for MFA", async () => {
    const locked = vi.fn<() => Promise<void>>().mockRejectedValue({ code: "locked", message: "locked" });
    await expect(runGuarded(locked, null, prompts({ unlock: [false] }))).rejects.toMatchObject({ code: "locked" });
    const mfa = vi.fn<() => Promise<void>>().mockRejectedValue({ code: "mfa_required", message: "mfa" });
    const asked = prompts({ mfa: [true] });
    await expect(runGuarded(mfa, null, asked)).rejects.toMatchObject({ code: "mfa_required" });
    expect(asked.mfaCalls).toEqual([]);
  });

  it("stops after a bounded number of prompts", async () => {
    const locked = vi.fn<() => Promise<void>>().mockRejectedValue({ code: "locked", message: "still locked" });
    const asked = prompts({ unlock: [true, true, true, true, true] });
    await expect(runGuarded(locked, null, asked)).rejects.toMatchObject({ code: "locked" });
    expect(asked.unlockCalls).toBe(MAX_PROMPTS_PER_CALL);
  });

  it("passes other errors through untouched", async () => {
    const failing = vi.fn<() => Promise<void>>().mockRejectedValue({ code: "not_found", message: "gone" });
    const asked = prompts({});
    await expect(runGuarded(failing, "a", asked)).rejects.toMatchObject({ code: "not_found", message: "gone" });
    expect(asked.unlockCalls + asked.mfaCalls.length).toBe(0);
  });

  it("wraps every command and forwards the complete arguments, nulls included", async () => {
    const calls: [string, unknown][] = [];
    const base = Object.fromEntries(
      COMMAND_NAMES.map((name) => [
        name,
        (args?: unknown) => {
          calls.push([name, args]);
          return Promise.resolve(name);
        },
      ]),
    ) as unknown as Backend;
    base.listen = () => () => undefined;
    const guarded = withSessionGuard(base, prompts({}));
    for (const name of COMMAND_NAMES) {
      const method = guarded[name] as (args?: unknown) => Promise<unknown>;
      const args = argsFor(name);
      await expect(method(args)).resolves.toBe(name);
      const [calledName, received] = calls.at(-1) ?? [];
      expect(calledName).toBe(name);
      expect(received).toStrictEqual(args);
    }
    expect(calls.map(([name]) => name)).toEqual(COMMAND_NAMES);
  });
});

describe("tauri backend", () => {
  beforeEach(() => {
    invoke.mockReset();
    listen.mockReset();
  });

  it("invokes every command by name with snake_case arguments", async () => {
    invoke.mockResolvedValue("result");
    const backend = createTauriBackend();
    await backend.list_objects({ account_id: "a", bucket: "b", prefix: "p/", flat: false, token: null });
    expect(invoke).toHaveBeenLastCalledWith("list_objects", {
      account_id: "a",
      bucket: "b",
      prefix: "p/",
      flat: false,
      token: null,
    });
    await backend.sync_analyze({
      account_id: "a",
      local_root: "/x",
      s3_prefix: "s3://b/",
      direction: "bidirectional",
      delete: true,
    });
    expect(invoke).toHaveBeenLastCalledWith("sync_analyze", {
      account_id: "a",
      local_root: "/x",
      s3_prefix: "s3://b/",
      direction: "bidirectional",
      delete: true,
    });
    await backend.security_status();
    expect(invoke).toHaveBeenLastCalledWith("security_status", undefined);
    for (const name of COMMAND_NAMES) {
      const method = backend[name] as (args?: unknown) => Promise<unknown>;
      const args = argsFor(name);
      await method(args);
      expect(invoke.mock.calls.at(-1)).toStrictEqual([name, args]);
    }
  });

  it("normalises rejections to BbError", async () => {
    invoke.mockRejectedValueOnce({ code: "auth", message: "SignatureDoesNotMatch" });
    const backend = createTauriBackend();
    await expect(backend.list_buckets({ account_id: "a" })).rejects.toMatchObject({
      code: "auth",
      message: "SignatureDoesNotMatch",
    });
    invoke.mockRejectedValueOnce("command not found");
    await expect(backend.app_info()).rejects.toBeInstanceOf(BackendError);
  });

  it("subscribes to events and unlistens, even when unsubscribed before listen resolves", async () => {
    const unlisten = vi.fn();
    let deliver: EventHandler | null = null;
    let resolveListen: ((fn: () => void) => void) | null = null;
    listen.mockImplementation((_name, handler) => {
      deliver = handler;
      return new Promise((resolve) => {
        resolveListen = resolve;
      });
    });
    const backend = createTauriBackend();
    const received: unknown[] = [];
    const stop = backend.listen("log://entry", (entry) => received.push(entry));
    expect(listen).toHaveBeenCalledWith("log://entry", expect.any(Function));
    const send = deliver as EventHandler | null;
    send?.({ payload: { id: 1 } });
    expect(received).toEqual([{ id: 1 }]);
    stop();
    send?.({ payload: { id: 2 } });
    expect(received).toEqual([{ id: 1 }]);
    const finish = resolveListen as ((fn: () => void) => void) | null;
    finish?.(unlisten);
    await Promise.resolve();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("reports listen failures", async () => {
    listen.mockRejectedValueOnce("event plugin missing");
    const onListenError = vi.fn();
    createTauriBackend({ onListenError }).listen("transfer://update", () => undefined);
    await vi.waitFor(() => expect(onListenError).toHaveBeenCalledWith(expect.any(BackendError)));
  });
});
