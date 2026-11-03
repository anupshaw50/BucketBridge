import type { Account, AccountInput, AccountUpdate, CredentialSource } from "../api/types";
import { validateEndpoint, validateRegion } from "./validation";

export const DEFAULT_REGION = "us-east-1";

export type SessionTokenMode = "keep" | "replace" | "clear";

/** Where the credentials of the account in the form come from ("profile" only when editing a linked AWS profile). */
export type SourceKind = "keys" | "role" | "sso" | "instance" | "profile";

export const DEFAULT_ROLE_SESSION_NAME = "bucketbridge";
export const DEFAULT_ROLE_MINUTES = 60;
export const MIN_ROLE_MINUTES = 15;
export const MAX_ROLE_MINUTES = 720;

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
  /** Where the credentials come from. */
  source_kind: SourceKind;
  /** AWS profile name (linked profiles are read-only). */
  profile: string;
  /** Assumed role: id of the account that signs the AssumeRole call. */
  role_source_account_id: string;
  role_arn: string;
  role_session_name: string;
  /** Session length in minutes, as typed. */
  role_duration_minutes: string;
  role_external_id: string;
  /** IAM Identity Center portal, account and role. */
  sso_start_url: string;
  sso_region: string;
  sso_session: string;
  sso_account_id: string;
  sso_role_name: string;
  /** Instance role: set once the machine was checked. */
  instance_checked: boolean;
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
    source_kind: "keys",
    profile: "",
    role_source_account_id: "",
    role_arn: "",
    role_session_name: DEFAULT_ROLE_SESSION_NAME,
    role_duration_minutes: String(DEFAULT_ROLE_MINUTES),
    role_external_id: "",
    sso_start_url: "",
    sso_region: "",
    sso_session: "",
    sso_account_id: "",
    sso_role_name: "",
    instance_checked: false,
  };
}

export function sourceKindOf(source: CredentialSource): SourceKind {
  switch (source.type) {
    case "static_keys":
      return "keys";
    case "aws_profile":
      return "profile";
    case "assume_role":
      return "role";
    case "sso":
      return "sso";
    case "instance_role":
      return "instance";
  }
}

/** Starting point of the edit dialog: secrets are never read back, so they start empty. */
export function draftFromAccount(account: Account): AccountDraft {
  const source = account.source;
  const role = source.type === "assume_role" ? source : null;
  const sso = source.type === "sso" ? source : null;
  return {
    ...emptyDraft(),
    source_kind: sourceKindOf(source),
    profile: source.type === "aws_profile" ? source.profile : "",
    role_source_account_id: role?.source_account_id ?? "",
    role_arn: role?.role_arn ?? "",
    role_session_name: role?.session_name ?? DEFAULT_ROLE_SESSION_NAME,
    role_duration_minutes: String(Math.round((role?.duration_secs ?? DEFAULT_ROLE_MINUTES * 60) / 60)),
    role_external_id: role?.external_id ?? "",
    sso_start_url: sso?.start_url ?? "",
    sso_region: sso?.sso_region ?? "",
    sso_session: sso?.sso_session ?? "",
    sso_account_id: sso?.account_id ?? "",
    sso_role_name: sso?.role_name ?? "",
    instance_checked: source.type === "instance_role",
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

const ROLE_ARN = /^arn:aws[a-z-]*:iam::\d{12}:role\/.+$/;
const ROLE_NAME_CHARS = /^[\w+=,.@-]+$/;

/** The credential source the form describes (undefined for the keys and profile kinds). */
export function sourceOfDraft(draft: AccountDraft): CredentialSource {
  switch (draft.source_kind) {
    case "role":
      return {
        type: "assume_role",
        source_account_id: draft.role_source_account_id,
        role_arn: draft.role_arn.trim(),
        session_name: draft.role_session_name.trim(),
        duration_secs: Math.round(Number(draft.role_duration_minutes) * 60),
        external_id: blankToNull(draft.role_external_id),
        mfa_serial: blankToNull(draft.mfa_serial),
      };
    case "sso":
      return {
        type: "sso",
        start_url: draft.sso_start_url.trim(),
        sso_region: draft.sso_region.trim(),
        account_id: draft.sso_account_id.trim(),
        role_name: draft.sso_role_name.trim(),
        sso_session: blankToNull(draft.sso_session),
      };
    case "instance":
      return { type: "instance_role" };
    case "profile":
      return { type: "aws_profile", profile: draft.profile };
    case "keys":
      return { type: "static_keys" };
  }
}

/** Step 2 for a role: the source account, the role and its session settings. */
function validateRole(draft: AccountDraft, accounts: readonly Account[], editing: boolean): DraftErrors {
  const errors: DraftErrors = {};
  if (!editing && !accounts.some((account) => account.id === draft.role_source_account_id)) {
    errors.role_source_account_id = "Choose the account that signs the AssumeRole call.";
  }
  if (!ROLE_ARN.test(draft.role_arn.trim())) {
    errors.role_arn = "Enter the role ARN, for example arn:aws:iam::123456789012:role/Name.";
  }
  const name = draft.role_session_name.trim();
  if (name.length < 2 || name.length > 64 || !ROLE_NAME_CHARS.test(name)) {
    errors.role_session_name = "Use 2 to 64 letters, digits or any of _+=,.@-.";
  }
  const minutes = Number(draft.role_duration_minutes);
  if (!Number.isInteger(minutes) || minutes < MIN_ROLE_MINUTES || minutes > MAX_ROLE_MINUTES) {
    errors.role_duration_minutes = `Enter a whole number of minutes from ${MIN_ROLE_MINUTES} to ${MAX_ROLE_MINUTES}.`;
  }
  const external = draft.role_external_id.trim();
  if (external.length > 0 && (external.length < 2 || external.length > 1224 || !/^[\w+=,.@:/-]+$/.test(external))) {
    errors.role_external_id = "Use 2 to 1224 letters, digits or any of _+=,.@:/-.";
  }
  const serial = draft.mfa_serial.trim();
  if (serial.length > 256 || /\s/.test(serial)) {
    errors.mfa_serial = "Enter the device ARN or serial number without spaces.";
  }
  return errors;
}

/** Step 2 for IAM Identity Center. */
function validateSso(draft: AccountDraft): DraftErrors {
  const errors: DraftErrors = {};
  if (!draft.sso_start_url.trim().startsWith("https://")) {
    errors.sso_start_url = "Enter the portal start URL, starting with https://.";
  }
  const regionError = validateRegion(draft.sso_region.trim());
  if (regionError) {
    errors.sso_region = regionError;
  }
  if (!/^\d{12}$/.test(draft.sso_account_id.trim())) {
    errors.sso_account_id = "Sign in and choose an AWS account.";
  }
  if (draft.sso_role_name.trim().length === 0) {
    errors.sso_role_name = "Choose a role.";
  }
  return errors;
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
export function validateCredentials(
  draft: AccountDraft,
  original: Account | null,
  accounts: readonly Account[] = [],
): DraftErrors {
  switch (draft.source_kind) {
    case "role":
      return validateRole(draft, accounts, original !== null);
    case "sso":
      return validateSso(draft);
    case "instance":
      return original === null && !draft.instance_checked
        ? { instance_checked: "Check that this machine has a role first." }
        : {};
    case "profile":
      return {};
    case "keys":
      break;
  }
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
  if (draft.source_kind !== "keys") {
    return {
      label: draft.label.trim(),
      access_key_id: "",
      secret_access_key: "",
      session_token: null,
      region: draft.region.trim(),
      endpoint: blankToNull(draft.endpoint),
      path_style: draft.path_style,
      request_payer: draft.request_payer,
      mfa_serial: null,
      source: sourceOfDraft(draft),
    };
  }
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
  if (original.source.type !== "static_keys") {
    return toKeylessUpdate(original, draft);
  }
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

/** Update of an account whose credentials come from a profile, a role, SSO or the instance role: no keys to change. */
function toKeylessUpdate(original: Account, draft: AccountDraft): AccountUpdate {
  const label = draft.label.trim();
  const region = draft.region.trim();
  const endpoint = blankToNull(draft.endpoint);
  const source = sourceOfDraft(draft);
  const sourceChanged = JSON.stringify(normalizeSource(source)) !== JSON.stringify(normalizeSource(original.source));
  return {
    label: label !== original.label ? label : null,
    access_key_id: null,
    secret_access_key: null,
    session_token: null,
    clear_session_token: false,
    region: region !== original.region ? region : null,
    endpoint: endpoint !== null && endpoint !== original.endpoint ? endpoint : null,
    clear_endpoint: endpoint === null && original.endpoint !== null,
    path_style: draft.path_style !== original.path_style ? draft.path_style : null,
    request_payer: draft.request_payer !== original.request_payer ? draft.request_payer : null,
    mfa_serial: null,
    clear_mfa_serial: false,
    source: sourceChanged && original.source.type === "assume_role" ? source : null,
  };
}

/** Missing optional fields and nulls compare equal. */
function normalizeSource(source: CredentialSource): CredentialSource {
  if (source.type === "assume_role") {
    return { ...source, external_id: source.external_id ?? null, mfa_serial: source.mfa_serial ?? null };
  }
  return source;
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
    !update.clear_mfa_serial &&
    (update.source ?? null) === null
  );
}

/** What an account signs with, for the account list: the key id, or where the credentials come from. */
export function credentialsLabel(account: Account, accounts: readonly Account[] = []): string {
  const source = account.source;
  switch (source.type) {
    case "static_keys":
      return account.access_key_id;
    case "aws_profile":
      return `AWS profile ${source.profile}`;
    case "assume_role": {
      const name = source.role_arn.split("/").pop() ?? source.role_arn;
      const via = accounts.find((item) => item.id === source.source_account_id)?.label;
      return via === undefined ? `Role ${name}` : `Role ${name} through ${via}`;
    }
    case "sso":
      return `SSO ${source.role_name} in ${source.account_id}`;
    case "instance_role":
      return "Instance or container role";
  }
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
