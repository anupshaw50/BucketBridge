// Website hosting rules of S3, checked before a configuration is sent. The same
// rules as `WebsiteConfig::validate` in crates/core/src/bucketcfg.rs; the form
// shows the problems next to the fields and the mock backend rejects with them.

import type { RoutingRule, WebsiteConfig } from "../api/types";

/** Most routing rules a website configuration may have. */
export const MAX_ROUTING_RULES = 50;

const MAX_KEY_BYTES = 1024;

function bytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** Three digits whose first digit is one of `first`. */
function statusIn(code: string, first: string): boolean {
  return /^\d{3}$/.test(code) && first.includes(code.charAt(0));
}

/** Why `host` cannot be a redirect host, or null. */
function hostProblem(host: string): string | null {
  if (host.length === 0) {
    return "is empty";
  }
  if (host.includes("://") || host.includes("/")) {
    return `${JSON.stringify(host)} must be a host name without a scheme or path`;
  }
  if (/\s/.test(host) || host.length > 255) {
    return `${JSON.stringify(host)} is not a host name`;
  }
  return null;
}

function ruleProblems(rule: RoutingRule, number: number): string[] {
  const label = `routing rule ${number}`;
  const problems: string[] = [];
  if (rule.condition !== null) {
    const prefix = rule.condition.key_prefix_equals;
    const code = rule.condition.http_error_code_returned_equals;
    if ((prefix === null || prefix.length === 0) && code === null) {
      problems.push(`${label}: the condition needs a key prefix or an HTTP error code`);
    }
    if (code !== null && !statusIn(code, "45")) {
      problems.push(`${label}: the error code ${JSON.stringify(code)} must be a 4xx or 5xx HTTP status`);
    }
  }
  const redirect = rule.redirect;
  if (
    redirect.protocol === null &&
    redirect.host_name === null &&
    redirect.replace_key_prefix_with === null &&
    redirect.replace_key_with === null &&
    redirect.http_redirect_code === null
  ) {
    problems.push(`${label}: the redirect needs a protocol, host, replacement key or prefix, or redirect code`);
  }
  if (redirect.replace_key_prefix_with !== null && redirect.replace_key_with !== null) {
    problems.push(`${label}: a redirect replaces the key prefix or the whole key, not both`);
  }
  if (redirect.host_name !== null) {
    const problem = hostProblem(redirect.host_name);
    if (problem !== null) {
      problems.push(`${label}: the redirect host: ${problem}`);
    }
  }
  if (redirect.http_redirect_code !== null && !statusIn(redirect.http_redirect_code, "3")) {
    problems.push(`${label}: the redirect code ${JSON.stringify(redirect.http_redirect_code)} must be a 3xx HTTP status`);
  }
  return problems;
}

/** Every problem of a website configuration; an empty list means it is valid. */
export function websiteProblems(website: WebsiteConfig): string[] {
  const problems: string[] = [];
  if (website.type === "redirect") {
    const problem = hostProblem(website.host_name);
    if (problem !== null) {
      problems.push(`the redirect host: ${problem}`);
    }
    return problems;
  }
  if (website.index_document.length === 0) {
    problems.push("the index document is empty");
  } else if (website.index_document.includes("/")) {
    problems.push(`the index document ${JSON.stringify(website.index_document)} must not contain a slash`);
  } else if (bytes(website.index_document) > MAX_KEY_BYTES) {
    problems.push(`the index document is longer than ${MAX_KEY_BYTES} bytes`);
  }
  if (
    website.error_document !== null &&
    (website.error_document.length === 0 || bytes(website.error_document) > MAX_KEY_BYTES)
  ) {
    problems.push(`the error document must be a key of 1 to ${MAX_KEY_BYTES} bytes`);
  }
  if (website.routing_rules.length > MAX_ROUTING_RULES) {
    problems.push(
      `there are ${website.routing_rules.length} routing rules; S3 accepts at most ${MAX_ROUTING_RULES}`,
    );
  }
  website.routing_rules.forEach((rule, index) => problems.push(...ruleProblems(rule, index + 1)));
  return problems;
}
