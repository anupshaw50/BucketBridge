import type { UploadRule, UploadRuleMatch, UploadRulesSettings } from "../api/types";

/** The encryption choices of a rule, with the value the service expects. */
export const RULE_SSE_VALUES = ["AES256", "aws:kms", "aws:kms:dsse"] as const;

/** The storage classes offered in the rule editor. */
export const RULE_STORAGE_CLASSES = [
  "STANDARD",
  "REDUCED_REDUNDANCY",
  "STANDARD_IA",
  "ONEZONE_IA",
  "INTELLIGENT_TIERING",
  "GLACIER_IR",
  "GLACIER",
  "DEEP_ARCHIVE",
] as const;

/** A new, empty rule for `pattern`. */
export function newRule(pattern = ""): UploadRule {
  return {
    name: "",
    enabled: true,
    pattern,
    case_insensitive: true,
    content_type: null,
    cache_control: null,
    content_disposition: null,
    content_encoding: null,
    headers: {},
    storage_class: null,
    sse: null,
    sse_kms_key_id: null,
  };
}

const SPECIAL = /[.*+?^${}()|[\]\\/]/g;

/** Turns a glob (`*`, `**`, `?`, `[a-z]`, `{a,b}`, `\` escapes) into a regular expression, like the core's matcher. */
export function globToRegExp(pattern: string, caseInsensitive: boolean): RegExp | null {
  let out = "";
  let braces = 0;
  for (let i = 0; i < pattern.length; i += 1) {
    const c = pattern.charAt(i);
    if (c === "*") {
      if (pattern.charAt(i + 1) === "*") {
        i += 1;
        if (pattern.charAt(i + 1) === "/") {
          i += 1;
          out += "(?:.*/)?";
        } else {
          out += ".*";
        }
      } else {
        out += "[^/]*";
      }
    } else if (c === "?") {
      out += "[^/]";
    } else if (c === "[") {
      const end = pattern.indexOf("]", i + 2);
      if (end < 0) {
        return null;
      }
      let body = pattern.slice(i + 1, end);
      if (body.startsWith("!")) {
        body = `^${body.slice(1)}`;
      }
      out += `[${body.replace(/\\/g, "\\\\")}]`;
      i = end;
    } else if (c === "{") {
      braces += 1;
      out += "(?:";
    } else if (c === "}" && braces > 0) {
      braces -= 1;
      out += ")";
    } else if (c === "," && braces > 0) {
      out += "|";
    } else if (c === "\\") {
      i += 1;
      out += (pattern.charAt(i) || "\\").replace(SPECIAL, "\\$&");
    } else {
      out += c.replace(SPECIAL, "\\$&");
    }
  }
  if (braces > 0) {
    return null;
  }
  try {
    return new RegExp(`^${out}$`, caseInsensitive ? "i" : "");
  } catch {
    return null;
  }
}

/** The first enabled rule that matches `name` (a file name or key), as the core's `test_rules` finds it. */
export function findUploadRule(settings: UploadRulesSettings, name: string): UploadRuleMatch {
  const file = name.slice(name.lastIndexOf("/") + 1);
  for (const [index, rule] of settings.rules.entries()) {
    if (!rule.enabled) {
      continue;
    }
    const regex = globToRegExp(rule.pattern, rule.case_insensitive);
    if (regex?.test(rule.pattern.includes("/") ? name : file) === true) {
      return { index, rule };
    }
  }
  return { index: null, rule: null };
}

/** The values a rule sets, as label and value pairs for the list and the test result. */
export function ruleValues(rule: UploadRule): [string, string][] {
  const out: [string, string][] = [];
  const add = (label: string, value: string | null) => {
    if (value !== null && value !== "") {
      out.push([label, value]);
    }
  };
  add("Content-Type", rule.content_type);
  add("Cache-Control", rule.cache_control);
  add("Content-Disposition", rule.content_disposition);
  add("Content-Encoding", rule.content_encoding);
  for (const [name, value] of Object.entries(rule.headers)) {
    add(name, value);
  }
  add("Storage class", rule.storage_class);
  const key = rule.sse_kms_key_id ?? "";
  add("Encryption", rule.sse === null ? null : key === "" ? rule.sse : `${rule.sse} (${key})`);
  return out;
}

/** `Name: value` lines to a header map; the first line without a name or a value is reported by its number. */
export function parseHeaderLines(text: string): { headers: Record<string, string>; badLine: number | null } {
  const headers: Record<string, string> = {};
  let badLine: number | null = null;
  for (const [index, line] of text.split("\n").entries()) {
    if (line.trim() === "") {
      continue;
    }
    const colon = line.indexOf(":");
    const name = colon < 0 ? "" : line.slice(0, colon).trim();
    const value = colon < 0 ? "" : line.slice(colon + 1).trim();
    if (name === "" || value === "") {
      badLine ??= index + 1;
      continue;
    }
    headers[name] = value;
  }
  return { headers, badLine };
}

export function headerLines(headers: Record<string, string>): string {
  return Object.entries(headers)
    .map(([name, value]) => `${name}: ${value}`)
    .join("\n");
}
