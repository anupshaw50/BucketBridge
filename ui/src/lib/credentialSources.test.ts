import { describe, expect, it } from "vitest";
import type { Account } from "../api/types";
import { tr } from "../i18n";
import {
  credentialsLabel,
  draftFromAccount,
  emptyDraft,
  isEmptyUpdate,
  toAccountInput,
  toAccountUpdate,
  validateCredentials,
} from "./accounts";

const base: Account = {
  id: "base",
  label: "Base",
  access_key_id: "KEY",
  secret_ref: "ref",
  session_token_ref: null,
  region: "us-east-1",
  endpoint: null,
  path_style: false,
  request_payer: false,
  mfa_serial: null,
  source: { type: "static_keys" },
};

const role: Account = {
  ...base,
  id: "role",
  label: "Writer",
  access_key_id: "",
  secret_ref: "",
  source: {
    type: "assume_role",
    source_account_id: "base",
    role_arn: "arn:aws:iam::123456789012:role/Writer",
    session_name: "bucketbridge",
    duration_secs: 3600,
    external_id: "ext",
    mfa_serial: null,
  },
};

describe("role accounts", () => {
  it("validates the role, its session and the source account", () => {
    const draft = { ...emptyDraft(), source_kind: "role" as const };
    const errors = validateCredentials(draft, null, [base]);
    expect(errors.role_source_account_id).toBeDefined();
    expect(errors.role_arn).toBeDefined();
    const filled = {
      ...draft,
      role_source_account_id: "base",
      role_arn: "arn:aws:iam::123456789012:role/Writer",
      role_duration_minutes: "60",
    };
    expect(validateCredentials(filled, null, [base])).toEqual({});
    for (const minutes of ["14", "721", "abc", "1.5"]) {
      expect(validateCredentials({ ...filled, role_duration_minutes: minutes }, null, [base]).role_duration_minutes).toBeDefined();
    }
    for (const minutes of ["15", "720"]) {
      expect(validateCredentials({ ...filled, role_duration_minutes: minutes }, null, [base])).toEqual({});
    }
    expect(validateCredentials({ ...filled, role_session_name: "a" }, null, [base]).role_session_name).toBeDefined();
    expect(validateCredentials({ ...filled, role_arn: "arn:aws:s3:::bucket" }, null, [base]).role_arn).toBeDefined();
    expect(validateCredentials({ ...filled, mfa_serial: "has space" }, null, [base]).mfa_serial).toBeDefined();
  });

  it("builds a keyless input with the role source", () => {
    const draft = {
      ...emptyDraft(),
      label: "  Writer ",
      source_kind: "role" as const,
      role_source_account_id: "base",
      role_arn: " arn:aws:iam::123456789012:role/Writer ",
      role_duration_minutes: "90",
      role_external_id: " ext ",
      mfa_serial: "arn:aws:iam::123456789012:mfa/me",
    };
    const input = toAccountInput(draft);
    expect(input).toMatchObject({ label: "Writer", access_key_id: "", secret_access_key: "", session_token: null, mfa_serial: null });
    expect(input.source).toEqual({
      type: "assume_role",
      source_account_id: "base",
      role_arn: "arn:aws:iam::123456789012:role/Writer",
      session_name: "bucketbridge",
      duration_secs: 5400,
      external_id: "ext",
      mfa_serial: "arn:aws:iam::123456789012:mfa/me",
    });
  });

  it("updates only the role source, and nothing when nothing changed", () => {
    const draft = draftFromAccount(role);
    expect(draft.role_duration_minutes).toBe("60");
    expect(isEmptyUpdate(toAccountUpdate(role, draft))).toBe(true);
    const longer = toAccountUpdate(role, { ...draft, role_duration_minutes: "120" });
    expect(longer.source).toMatchObject({ type: "assume_role", duration_secs: 7200 });
    expect(longer.access_key_id).toBeNull();
    expect(longer.secret_access_key).toBeNull();
    expect(isEmptyUpdate(longer)).toBe(false);
  });
});

describe("other sources", () => {
  it("asks for an SSO portal, account and role", () => {
    const draft = { ...emptyDraft(), source_kind: "sso" as const };
    const errors = validateCredentials(draft, null);
    expect(Object.keys(errors).sort()).toEqual(["sso_account_id", "sso_region", "sso_role_name", "sso_start_url"]);
    const filled = {
      ...draft,
      sso_start_url: "https://example.awsapps.com/start",
      sso_region: "eu-west-1",
      sso_account_id: "123456789012",
      sso_role_name: "Admin",
      sso_session: "corp",
    };
    expect(validateCredentials(filled, null)).toEqual({});
    expect(toAccountInput(filled).source).toEqual({
      type: "sso",
      start_url: "https://example.awsapps.com/start",
      sso_region: "eu-west-1",
      account_id: "123456789012",
      role_name: "Admin",
      sso_session: "corp",
    });
    expect(validateCredentials({ ...filled, sso_start_url: "http://x" }, null).sso_start_url).toBeDefined();
  });

  it("needs the instance role to be checked when adding", () => {
    const draft = { ...emptyDraft(), source_kind: "instance" as const };
    expect(validateCredentials(draft, null).instance_checked).toBeDefined();
    expect(validateCredentials({ ...draft, instance_checked: true }, null)).toEqual({});
    expect(toAccountInput(draft).source).toEqual({ type: "instance_role" });
  });

  it("describes where each account's credentials come from", () => {
    expect(credentialsLabel(base)).toBe("KEY");
    expect(credentialsLabel(role, [base, role])).toBe(tr("Role {name} through {via}", { name: "Writer", via: "Base" }));
    expect(credentialsLabel(role)).toBe(tr("Role {name}", { name: "Writer" }));
    expect(credentialsLabel({ ...base, source: { type: "aws_profile", profile: "dev" } })).toBe(tr("AWS profile {profile}", { profile: "dev" }));
    expect(credentialsLabel({ ...base, source: { type: "instance_role" } })).toBe(tr("Instance or container role"));
    expect(
      credentialsLabel({
        ...base,
        source: { type: "sso", start_url: "https://x", sso_region: "r", account_id: "123456789012", role_name: "Admin" },
      }),
    ).toBe(tr("SSO {role} in {account}", { role: "Admin", account: "123456789012" }));
  });

  it("keeps a linked profile read-only", () => {
    const profile = { ...base, source: { type: "aws_profile", profile: "dev" } } as const;
    const draft = draftFromAccount(profile);
    expect(draft.source_kind).toBe("profile");
    expect(validateCredentials(draft, profile)).toEqual({});
    expect(isEmptyUpdate(toAccountUpdate(profile, draft))).toBe(true);
  });
});
