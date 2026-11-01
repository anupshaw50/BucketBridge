import type { Account, AccountInput, AccountUpdate } from "../api/types";
import { validateEndpoint, validateRegion } from "./validation";

export const DEFAULT_REGION = "us-east-1";

export type SessionTokenMode = "keep" | "replace" | "clear";

/** Form state of the add-account wizard and the edit-account dialog. */
export interface AccountDraft {
  label: string;
  endpoint: string;
  region: string;
  path_style: boolean;
  request_payer: boolean;
  access_key_id: string;
  secret_access_key: string;
  session_token: string;
  /** Edit only: what happens to an existing session token. */
  session_token_mode: SessionTokenMode;
  mfa_serial: string;
}

export type DraftErrors = Partial<Record<keyof AccountDraft, string>>;

export function emptyDraft(): AccountDraft {
  return {
    label: "",
    endpoint: "",
    region: DEFAULT_REGION,
    path_style: false,
    request_payer: false,
    access_key_id: "",
    secret_access_key: "",
    session_token: "",
    session_token_mode: "replace",
    mfa_serial: "",
  };
}

/** Starting point of the edit dialog: secrets are never read back, so they start empty. */
export function draftFromAccount(account: Account): AccountDraft {
  return {
    label: account.label,
    endpoint: account.endpoint ?? "",
    region: account.region,
    path_style: account.path_style,
    request_payer: account.request_payer,
    access_key_id: account.access_key_id,
    secret_access_key: "",
    session_token: "",
    session_token_mode: "keep",
    mfa_serial: account.mfa_serial ?? "",
  };
}

export function labelTaken(label: string, accounts: readonly Account[], exceptId: string | null): boolean {
  const wanted = label.trim().toLowerCase();
  return accounts.some((account) => account.id !== exceptId && account.label.trim().toLowerCase() === wanted);
}

/** Wizard step 1 (connection) and the matching fields of the edit dialog. */
export function validateConnection(
  draft: AccountDraft,
  accounts: readonly Account[],
  editingId: string | null,
): DraftErrors {
  const errors: DraftErrors = {};
  const label = draft.label.trim();
  if (label.length === 0) {
    errors.label = "Enter a label for this account.";
  } else if (label.length > 128) {
    errors.label = "Keep the label under 128 characters.";
  } else if (labelTaken(label, accounts, editingId)) {
    errors.label = "Another account already uses this label.";
  }
  const endpointError = validateEndpoint(draft.endpoint.trim());
  if (endpointError) {
    errors.endpoint = endpointError;
  }
  const regionError = validateRegion(draft.region.trim());
  if (regionError) {
    errors.region = regionError;
  }
  return errors;
}

/**
 * Wizard step 2 (credentials). When editing, an empty secret keeps the
 * current one, but a changed access key needs its matching new secret.
 */
export function validateCredentials(draft: AccountDraft, original: Account | null): DraftErrors {
  const errors: DraftErrors = {};
  const accessKey = draft.access_key_id.trim();
  if (accessKey.length === 0) {
    errors.access_key_id = "Enter the access key ID.";
  } else if (/\s/.test(accessKey)) {
    errors.access_key_id = "The access key ID cannot contain spaces.";
  }
  const secret = draft.secret_access_key.trim();
  if (original === null) {
    if (secret.length === 0) {
      errors.secret_access_key = "Enter the secret access key.";
    }
  } else if (secret.length === 0 && accessKey.length > 0 && accessKey !== original.access_key_id) {
    errors.secret_access_key = "A new access key ID needs its secret access key.";
  }
  if (secret.length > 0 && /\s/.test(secret)) {
    errors.secret_access_key = "The secret access key cannot contain spaces.";
  }
  const needsToken = original === null ? false : draft.session_token_mode === "replace";
  if (needsToken && draft.session_token.trim().length === 0) {
    errors.session_token = "Enter the new session token, or keep the current one.";
  }
  const serial = draft.mfa_serial.trim();
  if (serial.length > 256 || /\s/.test(serial)) {
    errors.mfa_serial = "Enter the device ARN or serial number without spaces.";
  }
  return errors;
}

function blankToNull(value: string): string | null {
  const trimmed = value.trim();
  return trimmed.length === 0 ? null : trimmed;
}

export function toAccountInput(draft: AccountDraft): AccountInput {
  return {
    label: draft.label.trim(),
    access_key_id: draft.access_key_id.trim(),
    secret_access_key: draft.secret_access_key.trim(),
    session_token: blankToNull(draft.session_token),
    region: draft.region.trim(),
    endpoint: blankToNull(draft.endpoint),
    path_style: draft.path_style,
    request_payer: draft.request_payer,
    mfa_serial: blankToNull(draft.mfa_serial),
  };
}

/** Builds the minimal update: unchanged fields stay `null` ("keep current value"). */
export function toAccountUpdate(original: Account, draft: AccountDraft): AccountUpdate {
  const label = draft.label.trim();
  const accessKey = draft.access_key_id.trim();
  const secret = draft.secret_access_key.trim();
  const region = draft.region.trim();
  const endpoint = blankToNull(draft.endpoint);
  const serial = blankToNull(draft.mfa_serial);
  const token = blankToNull(draft.session_token);
  return {
    label: label !== original.label ? label : null,
    access_key_id: accessKey !== original.access_key_id ? accessKey : null,
    secret_access_key: secret.length > 0 ? secret : null,
    session_token: draft.session_token_mode === "replace" && token !== null ? token : null,
    clear_session_token: draft.session_token_mode === "clear" && original.session_token_ref !== null,
    region: region !== original.region ? region : null,
    endpoint: endpoint !== null && endpoint !== original.endpoint ? endpoint : null,
    clear_endpoint: endpoint === null && original.endpoint !== null,
    path_style: draft.path_style !== original.path_style ? draft.path_style : null,
    request_payer: draft.request_payer !== original.request_payer ? draft.request_payer : null,
    mfa_serial: serial !== null && serial !== original.mfa_serial ? serial : null,
    clear_mfa_serial: serial === null && original.mfa_serial !== null,
  };
}

export function isEmptyUpdate(update: AccountUpdate): boolean {
  return (
    update.label === null &&
    update.access_key_id === null &&
    update.secret_access_key === null &&
    update.session_token === null &&
    !update.clear_session_token &&
    update.region === null &&
    update.endpoint === null &&
    !update.clear_endpoint &&
    update.path_style === null &&
    update.request_payer === null &&
    update.mfa_serial === null &&
    !update.clear_mfa_serial
  );
}

/** "AWS" or the endpoint host, for lists and the status bar. */
export function providerLabel(account: Account): string {
  if (account.endpoint === null) {
    return "AWS";
  }
  try {
    return new URL(account.endpoint).host;
  } catch {
    return account.endpoint;
  }
}
