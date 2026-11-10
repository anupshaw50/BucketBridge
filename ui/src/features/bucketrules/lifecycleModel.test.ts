import { afterEach, describe, expect, it } from "vitest";
import { clearCopiedRules, copiedRules, copyRules, emptyRule, mergeRules, normalizeRule, parseRules, rulesText } from "./lifecycleModel";

afterEach(clearCopiedRules);

describe("lifecycle rules model", () => {
  it("fills the defaults of a rule that omits fields", () => {
    const rule = normalizeRule({ id: "a", expiration: { days: 30 }, transitions: [{ days: 5, storage_class: "GLACIER" }] });
    expect(rule.id).toBe("a");
    expect(rule.status).toBe("enabled");
    expect(rule.filter).toEqual({ prefix: null, tags: [], object_size_greater_than: null, object_size_less_than: null });
    expect(rule.expiration).toEqual({ days: 30, date: null, expired_object_delete_marker: false });
    expect(rule.transitions).toEqual([{ days: 5, date: null, storage_class: "GLACIER" }]);
    expect(rule.noncurrent_version_expiration).toBeNull();
    expect(normalizeRule(null)).toEqual(emptyRule());
  });

  it("parses the editor text and reports what is not rules", () => {
    expect(parseRules("")).toEqual({ ok: true, config: { rules: [] } });
    expect(parseRules('{"rules":[{"id":"x"}]}')).toMatchObject({ ok: true, config: { rules: [{ id: "x", status: "enabled" }] } });
    expect(parseRules("{oops").ok).toBe(false);
    expect(parseRules("[]").ok).toBe(false);
    expect(parseRules('{"rules":3}').ok).toBe(false);
  });

  it("writes text that reads back to the same rules", () => {
    const config = { rules: [{ ...emptyRule(), id: "a", abort_incomplete_multipart_upload_days: 7 }] };
    expect(parseRules(rulesText(config))).toEqual({ ok: true, config });
  });

  it("renames a pasted rule whose id is taken, and never overwrites", () => {
    const rule = (id: string | null) => ({ ...emptyRule(), id });
    const merged = mergeRules([rule("a"), rule("a-copy")], [rule("a"), rule("a"), rule("b"), rule(null)]);
    expect(merged.map((entry) => entry.id)).toEqual(["a", "a-copy", "a-copy-2", "a-copy-3", "b", null]);
  });

  it("keeps a copy of the copied rules", () => {
    expect(copiedRules()).toBeNull();
    const rules = [{ ...emptyRule(), id: "a" }];
    copyRules(rules);
    rules[0]!.id = "changed";
    expect(copiedRules()?.[0]?.id).toBe("a");
  });
});
