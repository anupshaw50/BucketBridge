import { describe, expect, it } from "vitest";
import type { Account } from "../api/types";
import { tr } from "../i18n";
import { knownStartPrefix } from "./buckets";
import { draftFromAccount, emptyDraft, isEmptyUpdate, toAccountInput, toAccountUpdate, validateConnection } from "./accounts";

function account(overrides: Partial<Account> = {}): Account {
  return {
    id: "a",
    label: "Main",
    access_key_id: "AKIAEXAMPLE",
    secret_ref: "s",
    session_token_ref: null,
    region: "eu-west-1",
    endpoint: null,
    path_style: false,
    request_payer: false,
    mfa_serial: null,
    source: { type: "static_keys" },
    options: { accelerate: false, dual_stack: false, compat: { storage_classes: [], regions: [], single_deletes: false, no_multipart_copy: false, list_all_regions: false }, cannot_list_buckets: false, known_buckets: [], proxy: { type: "global" } },
    ...overrides,
  };
}

describe("account network options", () => {
  it("round-trips through the draft", () => {
    const original = account({ options: { accelerate: true, dual_stack: false, compat: { storage_classes: [], regions: [], single_deletes: false, no_multipart_copy: false, list_all_regions: false }, cannot_list_buckets: false, known_buckets: [], proxy: { type: "global" } } });
    const draft = draftFromAccount(original);
    expect(draft.accelerate).toBe(true);
    expect(draft.dual_stack).toBe(false);
    expect(isEmptyUpdate(toAccountUpdate(original, draft))).toBe(true);
    const changed = toAccountUpdate(original, { ...draft, dual_stack: true });
    expect(changed.options).toMatchObject({ accelerate: true, dual_stack: true });
    expect(isEmptyUpdate(changed)).toBe(false);
  });

  it("sends options with a new account only when one is on", () => {
    const draft = { ...emptyDraft(), label: "L", access_key_id: "K", secret_access_key: "S" };
    expect(toAccountInput(draft).options).toBeUndefined();
    expect(toAccountInput({ ...draft, dual_stack: true }).options).toMatchObject({ accelerate: false, dual_stack: true });
  });

  it("carries the provider settings and checks their lists", () => {
    const original = account({ endpoint: "https://s3.example.com" });
    const draft = draftFromAccount(original);
    const changed = toAccountUpdate(original, {
      ...draft,
      compat_storage_classes: ["HOT", " COLD "],
      compat_regions: ["r1", "r2"],
      single_deletes: true,
      list_all_regions: true,
    });
    expect(changed.options?.compat).toEqual({
      storage_classes: ["HOT", "COLD"],
      regions: ["r1", "r2"],
      single_deletes: true,
      no_multipart_copy: false,
      list_all_regions: true,
    });
    const base = { ...emptyDraft(), label: "L", endpoint: "https://s3.example.com" };
    expect(validateConnection({ ...base, compat_storage_classes: ["bad class"] }, [], null).compat_storage_classes).toBeDefined();
    expect(validateConnection({ ...base, compat_regions: ["Bad Region"] }, [], null).compat_regions).toBeDefined();
    expect(validateConnection({ ...base, list_all_regions: true }, [], null).compat_regions).toBe(
      tr("Name the regions to list buckets across."),
    );
    expect(validateConnection({ ...base, list_all_regions: true, compat_regions: ["r1"] }, [], null).compat_regions).toBeUndefined();
  });

  it("refuses the options with a custom endpoint and acceleration in China", () => {
    const base = { ...emptyDraft(), label: "L", region: "eu-west-1" };
    expect(validateConnection(base, [], null).accelerate).toBeUndefined();
    const custom = validateConnection({ ...base, endpoint: "https://s3.example.com", dual_stack: true }, [], null);
    expect(custom.accelerate).toBe(
      tr("Transfer Acceleration and dual-stack endpoints are for AWS only; clear the endpoint or turn them off."),
    );
    const china = validateConnection({ ...base, region: "cn-north-1", accelerate: true }, [], null);
    expect(china.accelerate).toBe(tr("S3 Transfer Acceleration is not available in the China regions."));
    expect(validateConnection({ ...base, region: "cn-north-1", dual_stack: true }, [], null).accelerate).toBeUndefined();
  });

  it("carries the known buckets and opens them in their folder", () => {
    const original = account();
    const draft = draftFromAccount(original);
    const changed = toAccountUpdate(original, {
      ...draft,
      cannot_list_buckets: true,
      known_buckets: ["drop-box/uploads", " archive "],
    });
    expect(changed.options?.cannot_list_buckets).toBe(true);
    expect(changed.options?.known_buckets).toEqual(["drop-box/uploads", "archive"]);
    const base = { ...emptyDraft(), label: "L" };
    expect(validateConnection({ ...base, known_buckets: ["a b"] }, [], null).known_buckets).toBeDefined();
    expect(validateConnection({ ...base, known_buckets: ["drop-box/in/"] }, [], null).known_buckets).toBeUndefined();
    const options = { cannot_list_buckets: true, known_buckets: ["drop-box/uploads", "archive", "other/x/"] };
    expect(knownStartPrefix(options, "drop-box")).toBe("uploads/");
    expect(knownStartPrefix(options, "archive")).toBe("");
    expect(knownStartPrefix(options, "other")).toBe("x/");
    expect(knownStartPrefix({ ...options, cannot_list_buckets: false }, "drop-box")).toBe("");
  });

  it("carries a custom proxy and checks it", () => {
    const original = account();
    const draft = draftFromAccount(original);
    expect(draft.proxy_mode).toBe("global");
    const custom = toAccountUpdate(original, {
      ...draft,
      proxy_mode: "custom",
      proxy_kind: "socks5",
      proxy_host: " proxy.example ",
      proxy_port: "1080",
      proxy_bypass: ["*.internal"],
    });
    expect(custom.options?.proxy).toEqual({ type: "custom", kind: "socks5", host: "proxy.example", port: 1080, bypass: ["*.internal"] });
    expect(toAccountUpdate(original, { ...draft, proxy_mode: "none" }).options?.proxy).toEqual({ type: "none" });
    const base = { ...emptyDraft(), label: "L", proxy_mode: "custom" as const };
    expect(validateConnection({ ...base, proxy_host: "" }, [], null).proxy_host).toBeDefined();
    expect(validateConnection({ ...base, proxy_host: "p", proxy_port: "70000" }, [], null).proxy_port).toBeDefined();
    expect(validateConnection({ ...base, proxy_host: "p", proxy_port: "8080" }, [], null).proxy_port).toBeUndefined();
    const roundTrip = draftFromAccount(account({ options: { ...original.options, proxy: { type: "custom", kind: "http", host: "h", port: 3128, bypass: [] } } }));
    expect(roundTrip).toMatchObject({ proxy_mode: "custom", proxy_host: "h", proxy_port: "3128" });
  });
});
