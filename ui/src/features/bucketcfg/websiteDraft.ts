// The form state of the website tab and its conversion to and from `WebsiteConfig`.
// Every input is text, and an empty input means "not set".

import type { RedirectProtocol, RoutingRule, WebsiteConfig } from "../../api/types";

export type WebsiteMode = "off" | "hosting" | "redirect";

/** The protocol select: "same" keeps the protocol of the request. */
export type ProtocolChoice = "same" | RedirectProtocol;

export interface RuleDraft {
  /** Stable key of the row while rules are added and removed. */
  id: number;
  keyPrefix: string;
  errorCode: string;
  protocol: ProtocolChoice;
  host: string;
  replacePrefix: string;
  replaceKey: string;
  redirectCode: string;
}

export interface WebsiteDraft {
  mode: WebsiteMode;
  index: string;
  error: string;
  rules: RuleDraft[];
  redirectHost: string;
  redirectProtocol: ProtocolChoice;
}

export const DEFAULT_INDEX_DOCUMENT = "index.html";

let nextRuleId = 1;

export function newRule(): RuleDraft {
  nextRuleId += 1;
  return {
    id: nextRuleId,
    keyPrefix: "",
    errorCode: "",
    protocol: "same",
    host: "",
    replacePrefix: "",
    replaceKey: "",
    redirectCode: "",
  };
}

function blankToNull(text: string): string | null {
  return text.trim().length === 0 ? null : text.trim();
}

function ruleOf(draft: RuleDraft): RoutingRule {
  const keyPrefix = blankToNull(draft.keyPrefix);
  const errorCode = blankToNull(draft.errorCode);
  return {
    condition:
      keyPrefix === null && errorCode === null
        ? null
        : { key_prefix_equals: keyPrefix, http_error_code_returned_equals: errorCode },
    redirect: {
      protocol: draft.protocol === "same" ? null : draft.protocol,
      host_name: blankToNull(draft.host),
      replace_key_prefix_with: blankToNull(draft.replacePrefix),
      replace_key_with: blankToNull(draft.replaceKey),
      http_redirect_code: blankToNull(draft.redirectCode),
    },
  };
}

function ruleDraftOf(rule: RoutingRule): RuleDraft {
  return {
    ...newRule(),
    keyPrefix: rule.condition?.key_prefix_equals ?? "",
    errorCode: rule.condition?.http_error_code_returned_equals ?? "",
    protocol: rule.redirect.protocol ?? "same",
    host: rule.redirect.host_name ?? "",
    replacePrefix: rule.redirect.replace_key_prefix_with ?? "",
    replaceKey: rule.redirect.replace_key_with ?? "",
    redirectCode: rule.redirect.http_redirect_code ?? "",
  };
}

/** The form state for what the bucket has now. */
export function draftOf(website: WebsiteConfig | null): WebsiteDraft {
  const empty: WebsiteDraft = {
    mode: "off",
    index: DEFAULT_INDEX_DOCUMENT,
    error: "",
    rules: [],
    redirectHost: "",
    redirectProtocol: "same",
  };
  if (website === null) {
    return empty;
  }
  if (website.type === "redirect") {
    return { ...empty, mode: "redirect", redirectHost: website.host_name, redirectProtocol: website.protocol ?? "same" };
  }
  return {
    ...empty,
    mode: "hosting",
    index: website.index_document,
    error: website.error_document ?? "",
    rules: website.routing_rules.map(ruleDraftOf),
  };
}

/** The configuration the form describes; `null` when hosting is off. */
export function websiteOf(draft: WebsiteDraft): WebsiteConfig | null {
  switch (draft.mode) {
    case "off":
      return null;
    case "redirect":
      return {
        type: "redirect",
        host_name: draft.redirectHost.trim(),
        protocol: draft.redirectProtocol === "same" ? null : draft.redirectProtocol,
      };
    case "hosting":
      return {
        type: "hosting",
        index_document: draft.index.trim(),
        error_document: blankToNull(draft.error),
        routing_rules: draft.rules.map(ruleOf),
      };
  }
}

/** JSON with sorted keys, to compare configurations whatever the order of their fields. */
function canonical(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map(canonical).join(",")}]`;
  }
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${canonical(item)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

export function sameWebsite(a: WebsiteConfig | null, b: WebsiteConfig | null): boolean {
  return canonical(a) === canonical(b);
}
