import { tr } from "../i18n";
import type { BbError, BbErrorCode } from "./types";

export const BB_ERROR_CODES: readonly BbErrorCode[] = [
  "usage",
  "auth",
  "locked",
  "mfa_required",
  "not_found",
  "conflict",
  "remote",
  "io",
  "cancelled",
  "internal",
];

const CODE_SET: ReadonlySet<string> = new Set(BB_ERROR_CODES);

export function isBbErrorCode(value: unknown): value is BbErrorCode {
  return typeof value === "string" && CODE_SET.has(value);
}

/**
 * The error value every backend call rejects with. It is a real `Error` (so it
 * carries a stack and can be thrown) and structurally a contract `BbError`.
 */
export class BackendError extends Error implements BbError {
  readonly code: BbErrorCode;

  constructor(code: BbErrorCode, message: string) {
    super(message);
    this.name = "BackendError";
    this.code = code;
  }

  toJSON(): BbError {
    return { code: this.code, message: this.message };
  }
}

/**
 * Text of an error code when the backend sent no message of its own. The
 * entries are functions so that the sentence is taken in the language the
 * interface shows at that moment (`tr` reads it).
 */
const FALLBACK_MESSAGES: Record<BbErrorCode, () => string> = {
  usage: () => tr("The request was not valid."),
  auth: () => tr("Access was denied. Check the account credentials."),
  locked: () => tr("Stored secrets are locked. Unlock them with the master password."),
  mfa_required: () => tr("This account needs a multi-factor authentication code."),
  not_found: () => tr("The item no longer exists."),
  conflict: () => tr("The operation conflicts with existing data."),
  remote: () => tr("The storage service or the network reported an error."),
  io: () => tr("A local file system operation failed."),
  cancelled: () => tr("The operation was cancelled."),
  internal: () => tr("An unexpected internal error occurred."),
};

function messageOrFallback(code: BbErrorCode, message: unknown): string {
  if (typeof message === "string" && message.trim() !== "") {
    return message;
  }
  return FALLBACK_MESSAGES[code]();
}

function fromRecord(value: Record<string, unknown>): BackendError | null {
  const code = value.code;
  if (isBbErrorCode(code)) {
    return new BackendError(code, messageOrFallback(code, value.message));
  }
  if (typeof value.message === "string" && value.message.trim() !== "") {
    return new BackendError("internal", value.message);
  }
  return null;
}

/**
 * Normalises anything a command can reject with into a `BackendError`.
 * Tauri rejects with the serialized Rust error (an object), but a missing
 * command, a plugin failure or a bug can produce strings or `Error`s.
 */
export function toBbError(error: unknown): BackendError {
  if (error instanceof BackendError) {
    return error;
  }
  if (typeof error === "string") {
    const trimmed = error.trim();
    if (trimmed.startsWith("{")) {
      try {
        const parsed: unknown = JSON.parse(trimmed);
        if (parsed !== null && typeof parsed === "object") {
          const normalised = fromRecord(parsed as Record<string, unknown>);
          if (normalised) {
            return normalised;
          }
        }
      } catch {
        // Not JSON: fall through and use the text as the message.
      }
    }
    return new BackendError("internal", messageOrFallback("internal", error));
  }
  if (error instanceof Error) {
    const code: unknown = (error as Error & { code?: unknown }).code;
    if (isBbErrorCode(code)) {
      return new BackendError(code, messageOrFallback(code, error.message));
    }
    return new BackendError("internal", messageOrFallback("internal", error.message));
  }
  if (error !== null && typeof error === "object") {
    const normalised = fromRecord(error as Record<string, unknown>);
    if (normalised) {
      return normalised;
    }
  }
  return new BackendError("internal", FALLBACK_MESSAGES.internal());
}

/** Text suitable for the status bar or a notification. */
export function errorText(error: unknown): string {
  return toBbError(error).message;
}

export function hasCode(error: unknown, code: BbErrorCode): boolean {
  return toBbError(error).code === code;
}
