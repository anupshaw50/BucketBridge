// The IAM commands of the mock backend (docs/desktop-api.md, "IAM (T02)"): users, policies, access
// keys and login profiles per AWS account, in memory. The rules (names, typed confirmation, one
// secret that is shown once) follow the real host.

import type { CommandArgs } from "../api/backend";
import { BackendError } from "../api/errors";
import type {
  Account,
  IamAccessKey,
  IamDeleteOutcome,
  IamDeleteStep,
  IamInlinePolicy,
  IamLoginProfile,
  IamManagedPolicy,
  IamNewAccessKey,
  IamStatus,
  IamUser,
  IamUserDetails,
  PolicyIssue,
} from "../api/types";
import { nowIso, seededBytes, toHex } from "./util";

export const IAM_NOT_AVAILABLE = "IAM is not available for this endpoint";

const NAME = /^[A-Za-z0-9_+=,.@-]+$/;

interface UserRecord {
  user: IamUser;
  managed: string[];
  inline: Map<string, string>;
  keys: IamAccessKey[];
  profile: IamLoginProfile | null;
  groups: string[];
}

const CATALOG: IamManagedPolicy[] = [
  { name: "ReadOnlyAccess", arn: "arn:aws:iam::aws:policy/ReadOnlyAccess", attachment_count: 12, description: "Read-only access to AWS services" },
  { name: "AmazonS3FullAccess", arn: "arn:aws:iam::aws:policy/AmazonS3FullAccess", attachment_count: 4, description: "Full access to S3" },
  { name: "AmazonS3ReadOnlyAccess", arn: "arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess", attachment_count: 9, description: "Read-only access to S3" },
];

function fail(code: "usage" | "not_found" | "conflict", message: string): never {
  throw new BackendError(code, message);
}

/** Checks a user policy like the host: valid JSON object with Statement, Effect, Action and Resource. */
export function checkUserPolicy(text: string): PolicyIssue[] {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    return [{ path: "", message: `not valid JSON: ${String(error)}` }];
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return [{ path: "", message: "the policy must be a JSON object" }];
  }
  const doc = value as Record<string, unknown>;
  const issues: PolicyIssue[] = [];
  if (doc.Version !== "2012-10-17" && doc.Version !== "2008-10-17") {
    issues.push({ path: "Version", message: 'missing or not a policy language version; use "2012-10-17"' });
  }
  const raw = doc.Statement;
  const statements = Array.isArray(raw) ? raw : raw === undefined ? null : [raw];
  if (statements === null || statements.length === 0) {
    issues.push({ path: "Statement", message: "missing" });
    return issues;
  }
  statements.forEach((statement, index) => {
    const path = `Statement[${index}]`;
    if (typeof statement !== "object" || statement === null) {
      issues.push({ path, message: "must be an object" });
      return;
    }
    const entry = statement as Record<string, unknown>;
    if (entry.Effect !== "Allow" && entry.Effect !== "Deny") {
      issues.push({ path: `${path}.Effect`, message: 'must be "Allow" or "Deny"' });
    }
    if (!("Action" in entry) && !("NotAction" in entry)) {
      issues.push({ path, message: "Action or NotAction is missing" });
    }
    if (!("Resource" in entry) && !("NotResource" in entry)) {
      issues.push({ path, message: "Resource or NotResource is missing" });
    }
  });
  return issues;
}

export interface IamHost {
  account(id: string): Account;
  requireAccess(account: Account): void;
  log(message: string): void;
}

export class IamMock {
  private readonly host: IamHost;
  private readonly byAccount = new Map<string, Map<string, UserRecord>>();
  private counter = 0;

  constructor(host: IamHost) {
    this.host = host;
  }

  status(args: CommandArgs["iam_status"]): IamStatus {
    const account = this.host.account(args.account_id);
    const supported = account.endpoint === null;
    return { supported, message: supported ? null : IAM_NOT_AVAILABLE };
  }

  private users(accountId: string): Map<string, UserRecord> {
    const account = this.host.account(accountId);
    this.host.requireAccess(account);
    if (account.endpoint !== null) {
      fail("usage", IAM_NOT_AVAILABLE);
    }
    let users = this.byAccount.get(accountId);
    if (users === undefined) {
      users = new Map();
      this.byAccount.set(accountId, users);
    }
    return users;
  }

  private record(accountId: string, name: string): UserRecord {
    const found = this.users(accountId).get(name);
    if (found === undefined) {
      fail("not_found", `IAM GetUser: The user with name ${name} cannot be found.`);
    }
    return found;
  }

  private checkName(name: string): void {
    if (name.length === 0 || name.length > 64 || !NAME.test(name)) {
      fail("usage", "a user name is 1 to 64 letters, digits and the characters _+=,.@-");
    }
  }

  private typed(what: string, expected: string, typed: string): void {
    if (typed !== expected) {
      fail("usage", `${what} needs a typed confirmation: type ${expected} to confirm`);
    }
  }

  listUsers(args: CommandArgs["iam_list_users"]): IamUser[] {
    return [...this.users(args.account_id).values()].map((r) => ({ ...r.user })).sort((a, b) => a.name.localeCompare(b.name));
  }

  createUser(args: CommandArgs["iam_create_user"]): IamUser {
    const users = this.users(args.account_id);
    this.checkName(args.name);
    if (users.has(args.name)) {
      fail("conflict", `IAM CreateUser: User with name ${args.name} already exists.`);
    }
    const path = args.path ?? "/";
    this.counter += 1;
    const user: IamUser = {
      name: args.name,
      arn: `arn:aws:iam::123456789012:user${path}${args.name}`,
      user_id: `AIDA${toHex(seededBytes(args.name, 8)).toUpperCase()}`,
      path,
      created: nowIso(),
      password_last_used: null,
    };
    users.set(args.name, { user, managed: [], inline: new Map(), keys: [], profile: null, groups: [] });
    this.host.log(`created IAM user ${args.name}`);
    return { ...user };
  }

  updateUser(args: CommandArgs["iam_update_user"]): IamUser {
    const users = this.users(args.account_id);
    const record = this.record(args.account_id, args.name);
    if (args.new_name === null && args.new_path === null) {
      fail("usage", "nothing to change");
    }
    const name = args.new_name ?? args.name;
    this.checkName(name);
    if (name !== args.name && users.has(name)) {
      fail("conflict", `IAM UpdateUser: User with name ${name} already exists.`);
    }
    const path = args.new_path ?? record.user.path;
    record.user = { ...record.user, name, path, arn: `arn:aws:iam::123456789012:user${path}${name}` };
    users.delete(args.name);
    users.set(name, record);
    return { ...record.user };
  }

  deleteUser(args: CommandArgs["iam_delete_user"]): IamDeleteOutcome {
    const users = this.users(args.account_id);
    this.typed("deleting a user", args.name, args.confirm);
    const record = this.record(args.account_id, args.name);
    const steps: IamDeleteStep[] = [
      ...record.keys.map((k) => ({ kind: "access_key", target: k.access_key_id, ok: true, error: null })),
      ...[...record.inline.keys()].map((n) => ({ kind: "inline_policy", target: n, ok: true, error: null })),
      ...record.managed.map((arn) => ({ kind: "managed_policy", target: arn, ok: true, error: null })),
      { kind: "login_profile", target: "", ok: true, error: null },
      ...record.groups.map((g) => ({ kind: "group_membership", target: g, ok: true, error: null })),
      { kind: "user", target: args.name, ok: true, error: null },
    ];
    users.delete(args.name);
    return { user_name: args.name, steps, deleted: true };
  }

  details(args: CommandArgs["iam_user_details"]): IamUserDetails {
    const r = this.record(args.account_id, args.name);
    return {
      user: { ...r.user },
      managed_policies: {
        type: "loaded",
        value: r.managed.map((arn) => CATALOG.find((p) => p.arn === arn) ?? { name: arn.split("/").pop() ?? arn, arn, attachment_count: null, description: null }),
      },
      inline_policies: { type: "loaded", value: [...r.inline.keys()].sort() },
      access_keys: { type: "loaded", value: r.keys.map((k) => ({ ...k })) },
      login_profile: { type: "loaded", value: r.profile === null ? null : { ...r.profile } },
      groups: { type: "loaded", value: r.groups.map((name) => ({ name, arn: `arn:aws:iam::123456789012:group/${name}` })) },
    };
  }

  managedPolicies(args: CommandArgs["iam_list_managed_policies"]): IamManagedPolicy[] {
    this.users(args.account_id);
    return args.scope === "local" ? [] : CATALOG.map((p) => ({ ...p }));
  }

  attach(args: CommandArgs["iam_attach_policy"]): void {
    const r = this.record(args.account_id, args.user);
    if (!args.policy_arn.startsWith("arn:") || !args.policy_arn.includes(":policy/")) {
      fail("usage", "a managed policy ARN looks like arn:aws:iam::aws:policy/ReadOnlyAccess");
    }
    if (!r.managed.includes(args.policy_arn)) {
      r.managed.push(args.policy_arn);
    }
  }

  detach(args: CommandArgs["iam_detach_policy"]): void {
    const r = this.record(args.account_id, args.user);
    if (!r.managed.includes(args.policy_arn)) {
      fail("not_found", "IAM DetachUserPolicy: Policy was not found.");
    }
    r.managed = r.managed.filter((arn) => arn !== args.policy_arn);
  }

  getInline(args: CommandArgs["iam_get_inline_policy"]): IamInlinePolicy {
    const document = this.record(args.account_id, args.user).inline.get(args.name);
    if (document === undefined) {
      fail("not_found", `IAM GetUserPolicy: The user policy with name ${args.name} cannot be found.`);
    }
    return { name: args.name, document };
  }

  putInline(args: CommandArgs["iam_put_inline_policy"]): void {
    const r = this.record(args.account_id, args.user);
    if (args.name.length === 0 || !NAME.test(args.name)) {
      fail("usage", "a policy name is 1 to 128 letters, digits and the characters _+=,.@-");
    }
    const issues = checkUserPolicy(args.document);
    if (issues.length > 0) {
      fail("usage", `invalid policy: ${issues.map((i) => (i.path === "" ? i.message : `${i.path}: ${i.message}`)).join("; ")}`);
    }
    r.inline.set(args.name, JSON.stringify(JSON.parse(args.document), null, 2));
  }

  deleteInline(args: CommandArgs["iam_delete_inline_policy"]): void {
    this.typed("deleting a policy", args.name, args.confirm);
    const r = this.record(args.account_id, args.user);
    if (!r.inline.delete(args.name)) {
      fail("not_found", `IAM DeleteUserPolicy: The user policy with name ${args.name} cannot be found.`);
    }
  }

  createKey(args: CommandArgs["iam_create_access_key"]): IamNewAccessKey {
    const r = this.record(args.account_id, args.user);
    if (r.keys.length >= 2) {
      fail("conflict", "IAM CreateAccessKey: Cannot exceed quota for AccessKeysPerUser: 2");
    }
    this.counter += 1;
    const id = `AKIA${toHex(seededBytes(`key:${args.user}:${this.counter}`, 8)).toUpperCase()}`;
    const created = nowIso();
    r.keys.push({ access_key_id: id, status: "active", created });
    return {
      user_name: args.user,
      access_key_id: id,
      secret_access_key: toHex(seededBytes(`secret:${id}`, 20)),
      created,
    };
  }

  listKeys(args: CommandArgs["iam_list_access_keys"]): IamAccessKey[] {
    return this.record(args.account_id, args.user).keys.map((k) => ({ ...k }));
  }

  setKeyActive(args: CommandArgs["iam_set_access_key_active"]): void {
    const key = this.record(args.account_id, args.user).keys.find((k) => k.access_key_id === args.access_key_id);
    if (key === undefined) {
      fail("not_found", "IAM UpdateAccessKey: The Access Key with id was not found.");
    }
    key.status = args.active ? "active" : "inactive";
  }

  deleteKey(args: CommandArgs["iam_delete_access_key"]): void {
    this.typed("deleting an access key", args.access_key_id, args.confirm);
    const r = this.record(args.account_id, args.user);
    if (!r.keys.some((k) => k.access_key_id === args.access_key_id)) {
      fail("not_found", "IAM DeleteAccessKey: The Access Key with id was not found.");
    }
    r.keys = r.keys.filter((k) => k.access_key_id !== args.access_key_id);
  }

  createProfile(args: CommandArgs["iam_create_login_profile"]): void {
    const r = this.record(args.account_id, args.user);
    if (args.password.length === 0 || args.password.length > 128) {
      fail("usage", "a console password is 1 to 128 characters");
    }
    if (r.profile !== null) {
      fail("conflict", `IAM CreateLoginProfile: Login Profile for User ${args.user} already exists.`);
    }
    r.profile = { created: nowIso(), password_reset_required: args.reset_required };
  }

  updateProfile(args: CommandArgs["iam_update_login_profile"]): void {
    const r = this.record(args.account_id, args.user);
    if (args.password === null && args.reset_required === null) {
      fail("usage", "nothing to change");
    }
    if (r.profile === null) {
      fail("not_found", `IAM UpdateLoginProfile: Login Profile for User ${args.user} cannot be found.`);
    }
    r.profile = { ...r.profile, password_reset_required: args.reset_required ?? r.profile.password_reset_required };
  }

  deleteProfile(args: CommandArgs["iam_delete_login_profile"]): void {
    this.typed("deleting a login profile", args.user, args.confirm);
    const r = this.record(args.account_id, args.user);
    if (r.profile === null) {
      fail("not_found", `IAM DeleteLoginProfile: Login Profile for User ${args.user} cannot be found.`);
    }
    r.profile = null;
  }
}
