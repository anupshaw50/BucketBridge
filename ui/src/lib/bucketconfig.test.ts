import { describe, expect, it } from "vitest";
import type { WebsiteConfig } from "../api/types";
import { checkPolicy, compactPolicy, formatPolicy, issueText, MAX_POLICY_BYTES } from "./policy";
import { MAX_ROUTING_RULES, websiteProblems } from "./website";

const POLICY = JSON.stringify({
  Version: "2012-10-17",
  Statement: [
    {
      Sid: "PublicRead",
      Effect: "Allow",
      Principal: "*",
      Action: ["s3:GetObject"],
      Resource: "arn:aws:s3:::site/*",
    },
  ],
});

function messages(text: string): string[] {
  return checkPolicy(text).map(issueText);
}

describe("policy validation", () => {
  it("accepts a valid policy, also with one statement and every documented element", () => {
    expect(messages(POLICY)).toEqual([]);
    const single = { Version: "2008-10-17", Statement: { Effect: "Deny", Principal: "*", Action: "s3:*", Resource: "*" } };
    expect(messages(JSON.stringify(single))).toEqual([]);
    const full = {
      Version: "2012-10-17",
      Id: "p",
      Statement: [
        {
          Sid: "A",
          Effect: "Deny",
          NotPrincipal: { AWS: ["arn:aws:iam::123456789012:root"] },
          NotAction: "s3:GetObject",
          NotResource: ["arn:aws:s3:::b", "arn:aws:s3:::b/*"],
          Condition: { Bool: { "aws:SecureTransport": "false" }, NumericLessThan: { "s3:max-keys": 10 } },
        },
      ],
    };
    expect(messages(JSON.stringify(full))).toEqual([]);
  });

  it("reports syntax errors with their position, and an empty text", () => {
    expect(messages("")).toEqual(["the policy is empty"]);
    expect(messages('{\n  "Version": }')[0]).toMatch(/^not valid JSON: expected value at line 2 column 14$/);
    expect(messages("{} x")[0]).toMatch(/trailing characters at line 1 column 4/);
    expect(messages("[]")).toEqual(["the policy must be a JSON object, not an array"]);
  });

  it("reports every structural problem with its path", () => {
    const policy = JSON.stringify({
      Version: "2020-01-01",
      Statement: [
        { Effect: "allow", Principal: "*", NotPrincipal: "*", Action: [], Resource: ["ok", 7], Efect: 1 },
        { Sid: "S", Effect: "Allow", Principal: "someone", Action: "a", NotAction: "b" },
        { Sid: "S", Effect: "Deny", Principal: "*", Action: "a", Resource: "*", Condition: { Bool: "x" } },
      ],
    });
    expect(messages(policy)).toEqual([
      'Version: must be "2012-10-17" or "2008-10-17" (found "2020-01-01")',
      'Statement[0]: unknown element "Efect"; a statement has Sid, Effect, Principal, NotPrincipal, Action, NotAction, Resource, NotResource, Condition',
      'Statement[0].Effect: must be "Allow" or "Deny" (found "allow")',
      'Statement[0]: cannot have both "Principal" and "NotPrincipal"',
      "Statement[0].Action: must not be an empty list",
      "Statement[0].Resource[1]: must be a string, not a number",
      'Statement[1]: cannot have both "Action" and "NotAction"',
      'Statement[1]: needs "Resource" or "NotResource"',
      'Statement[1].Principal: must be "*" or an object such as {"AWS": "arn:aws:iam::123456789012:root"} (found "someone")',
      'Statement[2].Sid: "S" is already used by Statement[1]; Sid values must be unique',
      "Statement[2].Condition.Bool: must be an object that maps condition keys to values, not a string",
    ]);
  });

  it("reports repeated elements, a missing Version and a missing Statement", () => {
    expect(messages('{"Version": "2012-10-17", "Version": "2008-10-17", "Statement": []}')).toEqual([
      'duplicate element "Version"',
      "Statement: must contain at least one statement",
    ]);
    expect(messages("{}")).toEqual(['missing "Version"; add "Version": "2012-10-17"', 'missing "Statement"']);
  });

  it("measures the size limit without whitespace", () => {
    const resource = (padding: number) => `arn:aws:s3:::b/${"k".repeat(padding)}`;
    const make = (padding: number) =>
      JSON.stringify({
        Version: "2012-10-17",
        Statement: [{ Effect: "Allow", Principal: "*", Action: "s3:GetObject", Resource: resource(padding) }],
      });
    const base = make(0).length;
    const fits = make(MAX_POLICY_BYTES - base);
    expect(fits.length).toBe(MAX_POLICY_BYTES);
    expect(messages(fits)).toEqual([]);
    expect(messages(make(MAX_POLICY_BYTES - base + 1))).toEqual([
      "the policy is 20481 bytes without whitespace; S3 accepts at most 20480 bytes (20 KiB)",
    ]);
    // Indentation does not count.
    const pretty = formatPolicy(fits);
    expect("text" in pretty && pretty.text.length > MAX_POLICY_BYTES).toBe(true);
    expect("text" in pretty ? messages(pretty.text) : ["no text"]).toEqual([]);
  });
});

describe("policy formatting", () => {
  it("pretty-prints with two spaces and keeps the order of the keys", () => {
    const formatted = formatPolicy(POLICY);
    expect(formatted).toEqual({
      text: [
        "{",
        '  "Version": "2012-10-17",',
        '  "Statement": [',
        "    {",
        '      "Sid": "PublicRead",',
        '      "Effect": "Allow",',
        '      "Principal": "*",',
        '      "Action": [',
        '        "s3:GetObject"',
        "      ],",
        '      "Resource": "arn:aws:s3:::site/*"',
        "    }",
        "  ]",
        "}",
      ].join("\n"),
    });
    if ("text" in formatted) {
      expect(formatPolicy(formatted.text)).toEqual(formatted);
      expect(compactPolicy(formatted.text)).toEqual({ text: POLICY });
    }
    expect(formatPolicy("[1,{}]")).toEqual({ text: "[\n  1,\n  {}\n]" });
    expect(formatPolicy("{")).toEqual({
      issue: { path: "", message: "not valid JSON: unexpected end of input at line 1 column 2" },
    });
  });

  it("keeps numbers, escapes and unicode", () => {
    const text = '{"n":1.50,"big":18446744073709551615,"s":"a\\"b\\n\\u00e9","t":true,"f":false,"z":null}';
    const formatted = formatPolicy(text);
    expect("text" in formatted ? JSON.parse(formatted.text) : null).toEqual(JSON.parse(text));
    expect("text" in formatted ? formatted.text : "").toContain("18446744073709551615");
  });
});

describe("website rules", () => {
  const hosting = (patch: Partial<Extract<WebsiteConfig, { type: "hosting" }>> = {}): WebsiteConfig => ({
    type: "hosting",
    index_document: "index.html",
    error_document: null,
    routing_rules: [],
    ...patch,
  });
  const noRedirect = { protocol: null, host_name: null, replace_key_prefix_with: null, replace_key_with: null, http_redirect_code: null };

  it("accepts valid configurations", () => {
    expect(websiteProblems(hosting())).toEqual([]);
    expect(websiteProblems({ type: "redirect", host_name: "example.com", protocol: "https" })).toEqual([]);
    expect(
      websiteProblems(
        hosting({
          routing_rules: [
            {
              condition: { key_prefix_equals: "docs/", http_error_code_returned_equals: null },
              redirect: { ...noRedirect, replace_key_prefix_with: "documents/" },
            },
            { condition: null, redirect: { ...noRedirect, http_redirect_code: "301" } },
          ],
        }),
      ),
    ).toEqual([]);
  });

  it("reports every problem", () => {
    expect(websiteProblems(hosting({ index_document: "", error_document: "" }))).toEqual([
      "the index document is empty",
      "the error document must be a key of 1 to 1024 bytes",
    ]);
    expect(websiteProblems(hosting({ index_document: "dir/index.html" }))).toEqual([
      'the index document "dir/index.html" must not contain a slash',
    ]);
    const problems = websiteProblems(
      hosting({
        routing_rules: [
          { condition: { key_prefix_equals: null, http_error_code_returned_equals: null }, redirect: noRedirect },
          {
            condition: { key_prefix_equals: null, http_error_code_returned_equals: "200" },
            redirect: {
              ...noRedirect,
              replace_key_prefix_with: "a",
              replace_key_with: "b",
              host_name: "https://example.com/x",
              http_redirect_code: "404",
            },
          },
        ],
      }),
    );
    expect(problems).toEqual([
      "routing rule 1: the condition needs a key prefix or an HTTP error code",
      "routing rule 1: the redirect needs a protocol, host, replacement key or prefix, or redirect code",
      'routing rule 2: the error code "200" must be a 4xx or 5xx HTTP status',
      "routing rule 2: a redirect replaces the key prefix or the whole key, not both",
      'routing rule 2: the redirect host: "https://example.com/x" must be a host name without a scheme or path',
      'routing rule 2: the redirect code "404" must be a 3xx HTTP status',
    ]);
    expect(websiteProblems({ type: "redirect", host_name: "", protocol: null })).toEqual(["the redirect host: is empty"]);
    const rule = { condition: null, redirect: { ...noRedirect, host_name: "example.com" } };
    expect(websiteProblems(hosting({ routing_rules: Array.from({ length: MAX_ROUTING_RULES }, () => rule) }))).toEqual([]);
    expect(websiteProblems(hosting({ routing_rules: Array.from({ length: MAX_ROUTING_RULES + 1 }, () => rule) }))).toEqual([
      "there are 51 routing rules; S3 accepts at most 50",
    ]);
  });
});
