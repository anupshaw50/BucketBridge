// Account folders, the order of accounts, and the account file of the mock backend (docs/desktop-api.md,
// "Account folders, order, export and import"). The files an export writes are kept here as text, keyed by
// path, so that an import in the same session reads them back; `seedFile` lets a test put one in place.

import { BackendError } from "../api/errors";
import type {
  Account,
  AccountFolder,
  AccountLayout,
  BbErrorCode,
  CredentialSource,
  DuplicatePolicy,
  ExportAccountsOutcome,
  ImportDuplicate,
  ImportReport,
} from "../api/types";
import { checkSource } from "./credentialsMock";
import { type MockFs, normalizePath } from "./fs";
import { nowIso, uuid } from "./util";

const FORMAT = "bucketbridge-accounts";
const VERSION = 1;
const MAX_FOLDER_NAME = 100;
const MAX_ACCOUNTS = 1000;

type ExportedSource =
  | { type: "static_keys" }
  | { type: "aws_profile"; profile: string }
  | {
      type: "assume_role";
      source_account: string;
      role_arn: string;
      session_name: string;
      duration_secs: number;
      external_id?: string | null;
      mfa_serial?: string | null;
    }
  | { type: "sso"; start_url: string; sso_region: string; account_id: string; role_name: string; sso_session?: string | null }
  | { type: "instance_role" };

interface ExportedAccount {
  label: string;
  folder?: string | null;
  access_key_id: string;
  region: string;
  endpoint?: string | null;
  path_style: boolean;
  request_payer: boolean;
  mfa_serial?: string | null;
  source: ExportedSource;
}

interface AccountExport {
  format: string;
  version: number;
  exported_at: string;
  folders: string[];
  accounts: ExportedAccount[];
}

function fail(code: BbErrorCode, message: string): never {
  throw new BackendError(code, message);
}

function checkFolderName(raw: string): string {
  const name = raw.trim();
  if (name.length === 0) {
    fail("usage", "the folder name is empty");
  }
  if (name.length > MAX_FOLDER_NAME) {
    fail("usage", `the folder name must be at most ${MAX_FOLDER_NAME} characters`);
  }
  return name;
}

export class AccountFilesMock {
  private folders: AccountFolder[] = [];
  private readonly files = new Map<string, string>();

  /** The folders with the accounts that still exist, each account in one folder only. */
  list(accounts: readonly Account[]): AccountFolder[] {
    const live = new Set(accounts.map((account) => account.id));
    const seen = new Set<string>();
    return this.folders.map((folder) => ({
      name: folder.name,
      account_ids: folder.account_ids.filter((id) => live.has(id) && !seen.has(id) && (seen.add(id), true)),
    }));
  }

  /** Stores a layout; returns the accounts in their new order. */
  setLayout(accounts: readonly Account[], layout: AccountLayout): Account[] {
    const stored = new Set(accounts.map((account) => account.id));
    const given = new Set(layout.accounts);
    if (given.size !== layout.accounts.length || given.size !== stored.size || layout.accounts.some((id) => !stored.has(id))) {
      fail("conflict", "the accounts changed since the list was loaded: reload it and try again");
    }
    const names = new Set<string>();
    const placed = new Set<string>();
    const folders: AccountFolder[] = [];
    for (const folder of layout.folders) {
      const name = checkFolderName(folder.name);
      if (names.has(name.toLowerCase())) {
        fail("conflict", `a folder named "${name}" already exists`);
      }
      names.add(name.toLowerCase());
      for (const id of folder.account_ids) {
        if (!stored.has(id)) {
          fail("conflict", `folder "${name}" names an account that does not exist`);
        }
        if (placed.has(id)) {
          fail("usage", "an account can be in one folder only");
        }
        placed.add(id);
      }
      folders.push({ name, account_ids: [...folder.account_ids] });
    }
    this.folders = folders;
    const position = new Map(layout.accounts.map((id, index) => [id, index]));
    return [...accounts].sort((a, b) => (position.get(a.id) ?? 0) - (position.get(b.id) ?? 0));
  }

  /** Puts a file in place, as if an export had written it (for tests). */
  seedFile(path: string, text: string): void {
    this.files.set(path, text);
  }

  /** The text of a file that an export wrote (for tests). */
  read(path: string): string {
    return this.files.get(path) ?? fail("not_found", `there is no account file at ${path}`);
  }

  export(
    accounts: readonly Account[],
    args: { path: string; account_ids: string[] | null },
    fs: MockFs,
  ): ExportAccountsOutcome {
    const path = normalizePath(args.path);
    if (path === null) {
      fail("usage", "the account file path must be an absolute path");
    }
    const wanted = new Set<string>(args.account_ids ?? accounts.map((account) => account.id));
    for (const id of wanted) {
      if (!accounts.some((account) => account.id === id)) {
        fail("not_found", `there is no account "${id}"`);
      }
    }
    // A role pulls its source account into the file.
    for (let grew = true; grew; ) {
      grew = false;
      for (const account of accounts) {
        if (wanted.has(account.id) && account.source.type === "assume_role" && !wanted.has(account.source.source_account_id)) {
          wanted.add(account.source.source_account_id);
          grew = true;
        }
      }
    }
    const folders = this.list(accounts);
    const folderOf = (id: string) => folders.find((folder) => folder.account_ids.includes(id))?.name ?? null;
    const exported: ExportedAccount[] = accounts
      .filter((account) => wanted.has(account.id))
      .map((account) => ({
        label: account.label,
        folder: folderOf(account.id),
        access_key_id: account.access_key_id,
        region: account.region,
        endpoint: account.endpoint,
        path_style: account.path_style,
        request_payer: account.request_payer,
        mfa_serial: account.mfa_serial,
        source: exportedSource(account, accounts),
      }));
    const used = new Set(exported.map((account) => account.folder));
    const document: AccountExport = {
      format: FORMAT,
      version: VERSION,
      exported_at: nowIso(),
      folders: folders.filter((folder) => args.account_ids === null || used.has(folder.name)).map((folder) => folder.name),
      accounts: exported,
    };
    const text = `${JSON.stringify(document, null, 2)}\n`;
    this.files.set(path, text);
    fs.writeFile(path, { size: text.length, modified: nowIso() });
    return { path, accounts: exported.length, folders: document.folders.length };
  }

  /** Reads a file and adds its accounts through `add`; `existing` is the list before the import. */
  import(
    existing: Account[],
    args: { path: string; duplicates: DuplicatePolicy },
    add: (account: Account) => void,
  ): ImportReport {
    const path = normalizePath(args.path);
    if (path === null) {
      fail("usage", "the account file path must be an absolute path");
    }
    const text = this.files.get(path);
    if (text === undefined) {
      fail("not_found", `there is no account file at ${path}`);
    }
    const document = parse(text);
    const report: ImportReport = { imported: [], duplicates: [], skipped: [], folders_created: [] };
    const byLabel = new Map<string, string>();
    const members = new Map<string, string[]>();
    const roles = document.accounts.filter((account) => account.source.type === "assume_role");
    const others = document.accounts.filter((account) => account.source.type !== "assume_role");
    for (const incoming of [...others, ...roles]) {
      const label = incoming.label.trim();
      const problem = invalid(incoming);
      if (problem !== null) {
        report.skipped.push({ label, reason: problem });
        continue;
      }
      const same = existing.find((account) => sameAccount(account, incoming));
      if (same !== undefined) {
        byLabel.set(label.toLowerCase(), same.id);
        report.duplicates.push({ label, existing: same.label, reason: "same_account" });
        continue;
      }
      let finalLabel = label;
      const taken = existing.find((account) => account.label.trim().toLowerCase() === label.toLowerCase());
      if (taken !== undefined) {
        const duplicate: ImportDuplicate = { label, existing: taken.label, reason: "label" };
        if (args.duplicates === "skip") {
          byLabel.set(label.toLowerCase(), taken.id);
          report.duplicates.push(duplicate);
          continue;
        }
        report.duplicates.push(duplicate);
        let n = 2;
        while (existing.some((account) => account.label.toLowerCase() === `${label} (${n})`.toLowerCase())) {
          n += 1;
        }
        finalLabel = `${label} (${n})`;
      }
      let source: CredentialSource;
      try {
        source = importedSource(incoming.source, byLabel);
        checkSource(source, existing, null);
      } catch (raw) {
        report.duplicates = report.duplicates.filter((item) => !(item.label === label && item.reason === "label" && finalLabel !== label));
        report.skipped.push({ label, reason: raw instanceof Error ? raw.message : "invalid account" });
        continue;
      }
      const isStatic = source.type === "static_keys";
      const account: Account = {
        id: uuid(),
        label: finalLabel,
        access_key_id: isStatic ? incoming.access_key_id.trim() : "",
        secret_ref: "",
        session_token_ref: null,
        region: incoming.region,
        endpoint: incoming.endpoint ?? null,
        path_style: incoming.path_style,
        request_payer: incoming.request_payer,
        options: { accelerate: false, dual_stack: false, compat: { storage_classes: [], regions: [], single_deletes: false, no_multipart_copy: false, list_all_regions: false }, cannot_list_buckets: false, known_buckets: [], proxy: { type: "global" } },
        mfa_serial: source.type === "assume_role" ? (source.mfa_serial ?? null) : isStatic ? (incoming.mfa_serial ?? null) : null,
        source,
      };
      existing.push(account);
      add(account);
      byLabel.set(label.toLowerCase(), account.id);
      report.imported.push({
        id: account.id,
        label: finalLabel,
        renamed_from: finalLabel === label ? null : label,
        needs_secret: isStatic,
      });
      if (incoming.folder) {
        const name = checkFolderName(incoming.folder);
        members.set(name, [...(members.get(name) ?? []), account.id]);
      }
    }
    for (const raw of [...document.folders, ...members.keys()]) {
      const name = checkFolderName(raw);
      let folder = this.folders.find((item) => item.name.toLowerCase() === name.toLowerCase());
      if (folder === undefined) {
        folder = { name, account_ids: [] };
        this.folders.push(folder);
        report.folders_created.push(name);
      }
      folder.account_ids.push(...(members.get(folder.name) ?? []));
      members.delete(folder.name);
    }
    return report;
  }
}

function exportedSource(account: Account, accounts: readonly Account[]): ExportedSource {
  const source = account.source;
  if (source.type !== "assume_role") {
    return source;
  }
  const base = accounts.find((item) => item.id === source.source_account_id);
  return {
    type: "assume_role",
    source_account: base?.label ?? "",
    role_arn: source.role_arn,
    session_name: source.session_name,
    duration_secs: source.duration_secs,
    external_id: source.external_id ?? null,
    mfa_serial: source.mfa_serial ?? null,
  };
}

function importedSource(source: ExportedSource, byLabel: ReadonlyMap<string, string>): CredentialSource {
  if (source.type !== "assume_role") {
    return source;
  }
  const id = byLabel.get(source.source_account.trim().toLowerCase());
  if (id === undefined) {
    throw new Error(`its source account "${source.source_account}" is not in the file or here`);
  }
  return {
    type: "assume_role",
    source_account_id: id,
    role_arn: source.role_arn,
    session_name: source.session_name,
    duration_secs: source.duration_secs,
    external_id: source.external_id ?? null,
    mfa_serial: source.mfa_serial ?? null,
  };
}

function invalid(account: ExportedAccount): string | null {
  if (account.label.trim().length === 0) {
    return "the account label is empty";
  }
  if (account.source.type === "static_keys" && account.access_key_id.trim().length === 0) {
    return "the access key ID is empty";
  }
  if (!/^[a-z0-9-]{2,40}$/.test(account.region)) {
    return `"${account.region}" is not a region`;
  }
  return null;
}

function sameAccount(existing: Account, incoming: ExportedAccount): boolean {
  const a = existing.source;
  const b = incoming.source;
  if (a.type === "static_keys" && b.type === "static_keys") {
    return (
      existing.access_key_id === incoming.access_key_id.trim() &&
      (existing.endpoint ?? "") === (incoming.endpoint ?? "") &&
      existing.region === incoming.region
    );
  }
  if (a.type === "aws_profile" && b.type === "aws_profile") {
    return a.profile === b.profile && existing.region === incoming.region;
  }
  if (a.type === "sso" && b.type === "sso") {
    return a.start_url === b.start_url && a.account_id === b.account_id && a.role_name === b.role_name;
  }
  if (a.type === "instance_role" && b.type === "instance_role") {
    return existing.region === incoming.region && existing.endpoint === (incoming.endpoint ?? null);
  }
  if (a.type === "assume_role" && b.type === "assume_role") {
    return a.role_arn === b.role_arn;
  }
  return false;
}

function parse(text: string): AccountExport {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    fail("usage", "this is not a BucketBridge account file");
  }
  if (typeof value !== "object" || value === null || (value as { format?: unknown }).format !== FORMAT) {
    fail("usage", "this is not a BucketBridge account file");
  }
  const document = value as Partial<AccountExport>;
  if (typeof document.version !== "number") {
    fail("usage", "the account file has no version number");
  }
  if (document.version < 1 || document.version > VERSION) {
    fail("usage", `this account file has version ${document.version}, which this version of BucketBridge cannot read`);
  }
  if (!Array.isArray(document.accounts)) {
    fail("usage", "the account file is damaged");
  }
  if (document.accounts.length > MAX_ACCOUNTS) {
    fail("usage", `the account file holds more than ${MAX_ACCOUNTS} accounts`);
  }
  return { ...document, folders: document.folders ?? [] } as AccountExport;
}
