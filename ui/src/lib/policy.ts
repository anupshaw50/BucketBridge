// Bucket policy documents: the checks and the formatting that the backend's
// `validate_bucket_policy` and `format_bucket_policy` commands apply
// (crates/core/src/bucketcfg.rs). The mock backend simulates those commands
// with this module, so the UI behaves in the browser as it does in the app.

import type { PolicyIssue } from "../api/types";

/** Largest policy S3 accepts (20 KiB), measured without whitespace. */
export const MAX_POLICY_BYTES = 20 * 1024;

const POLICY_VERSIONS = ["2012-10-17", "2008-10-17"];
const POLICY_ELEMENTS = ["Id", "Statement", "Version"];
const STATEMENT_ELEMENTS = [
  "Sid",
  "Effect",
  "Principal",
  "NotPrincipal",
  "Action",
  "NotAction",
  "Resource",
  "NotResource",
  "Condition",
];
const MAX_DEPTH = 128;

/** A JSON value whose objects keep their keys in document order and repeated keys. */
type Json =
  | { type: "null" }
  | { type: "bool"; value: boolean }
  | { type: "number"; raw: string }
  | { type: "string"; value: string }
  | { type: "array"; items: Json[] }
  | { type: "object"; entries: [string, Json][] };

class JsonSyntaxError extends Error {}

function describe(value: Json): string {
  switch (value.type) {
    case "null":
      return "null";
    case "bool":
      return "a boolean";
    case "number":
      return "a number";
    case "string":
      return "a string";
    case "array":
      return "an array";
    case "object":
      return "an object";
  }
}

const NUMBER = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y;

/** Parses JSON text; a syntax error names its line and column, like the backend's. */
function parseJson(text: string): Json {
  let position = 0;

  const fail = (message: string, at = position): never => {
    const before = text.slice(0, at);
    const line = before.split("\n").length;
    const column = at - (before.lastIndexOf("\n") + 1) + 1;
    throw new JsonSyntaxError(`${message} at line ${line} column ${column}`);
  };
  const skip = () => {
    while (position < text.length && " \t\n\r".includes(text.charAt(position))) {
      position += 1;
    }
  };
  const string = (): string => {
    // The opening quote is at `position`.
    let out = "";
    position += 1;
    for (;;) {
      if (position >= text.length) {
        return fail("unexpected end of input while reading a string");
      }
      const char = text.charAt(position);
      if (char === '"') {
        position += 1;
        return out;
      }
      if (char < " ") {
        return fail("control character in a string");
      }
      if (char !== "\\") {
        out += char;
        position += 1;
        continue;
      }
      const escape = text.charAt(position + 1);
      const simple: Record<string, string> = {
        '"': '"',
        "\\": "\\",
        "/": "/",
        b: "\b",
        f: "\f",
        n: "\n",
        r: "\r",
        t: "\t",
      };
      if (escape in simple) {
        out += simple[escape];
        position += 2;
      } else if (escape === "u") {
        const hex = text.slice(position + 2, position + 6);
        if (!/^[0-9a-fA-F]{4}$/.test(hex)) {
          return fail("invalid unicode escape", position);
        }
        out += String.fromCharCode(Number.parseInt(hex, 16));
        position += 6;
      } else {
        return fail("invalid escape", position);
      }
    }
  };
  const value = (depth: number): Json => {
    if (depth > MAX_DEPTH) {
      return fail("recursion limit exceeded");
    }
    skip();
    const char = text.charAt(position);
    if (char === "{") {
      position += 1;
      const entries: [string, Json][] = [];
      skip();
      if (text.charAt(position) === "}") {
        position += 1;
        return { type: "object", entries };
      }
      for (;;) {
        skip();
        if (position >= text.length) {
          return fail("unexpected end of input");
        }
        if (text.charAt(position) !== '"') {
          return fail("key must be a string");
        }
        const key = string();
        skip();
        if (text.charAt(position) !== ":") {
          return position >= text.length ? fail("unexpected end of input") : fail("expected `:`");
        }
        position += 1;
        entries.push([key, value(depth + 1)]);
        skip();
        if (position >= text.length) {
          return fail("unexpected end of input");
        }
        const next = text.charAt(position);
        position += 1;
        if (next === "}") {
          return { type: "object", entries };
        }
        if (next !== ",") {
          return fail("expected `,` or `}`", position - 1);
        }
      }
    }
    if (char === "[") {
      position += 1;
      const items: Json[] = [];
      skip();
      if (text.charAt(position) === "]") {
        position += 1;
        return { type: "array", items };
      }
      for (;;) {
        items.push(value(depth + 1));
        skip();
        if (position >= text.length) {
          return fail("unexpected end of input");
        }
        const next = text.charAt(position);
        position += 1;
        if (next === "]") {
          return { type: "array", items };
        }
        if (next !== ",") {
          return fail("expected `,` or `]`", position - 1);
        }
      }
    }
    if (char === '"') {
      return { type: "string", value: string() };
    }
    for (const [literal, result] of [
      ["true", { type: "bool", value: true }],
      ["false", { type: "bool", value: false }],
      ["null", { type: "null" }],
    ] as const) {
      if (text.startsWith(literal, position)) {
        position += literal.length;
        return result;
      }
    }
    NUMBER.lastIndex = position;
    const number = NUMBER.exec(text);
    if (number !== null) {
      position += number[0].length;
      return { type: "number", raw: number[0] };
    }
    return position >= text.length ? fail("unexpected end of input") : fail("expected value");
  };

  const document = value(0);
  skip();
  if (position < text.length) {
    fail("trailing characters");
  }
  return document;
}

/** Parses policy text; the only issue of a document that is not JSON is its syntax error. */
function parsePolicy(text: string): { document: Json } | { issue: PolicyIssue } {
  if (text.trim().length === 0) {
    return { issue: { path: "", message: "the policy is empty" } };
  }
  try {
    return { document: parseJson(text) };
  } catch (error) {
    if (error instanceof JsonSyntaxError) {
      return { issue: { path: "", message: `not valid JSON: ${error.message}` } };
    }
    throw error;
  }
}

function serialize(value: Json, indent: string | null, level: number): string {
  const inner = indent === null ? "" : `\n${indent.repeat(level + 1)}`;
  const outer = indent === null ? "" : `\n${indent.repeat(level)}`;
  switch (value.type) {
    case "null":
      return "null";
    case "bool":
      return value.value ? "true" : "false";
    case "number":
      return value.raw;
    case "string":
      return JSON.stringify(value.value);
    case "array":
      if (value.items.length === 0) {
        return "[]";
      }
      return `[${value.items.map((item) => `${inner}${serialize(item, indent, level + 1)}`).join(",")}${outer}]`;
    case "object":
      if (value.entries.length === 0) {
        return "{}";
      }
      return `{${value.entries
        .map(([key, item]) => `${inner}${JSON.stringify(key)}:${indent === null ? "" : " "}${serialize(item, indent, level + 1)}`)
        .join(",")}${outer}}`;
  }
}

function utf8Length(text: string): number {
  return new TextEncoder().encode(text).length;
}

function first(entries: readonly [string, Json][], key: string): Json | undefined {
  return entries.find(([name]) => name === key)?.[1];
}

function childPath(path: string, key: string): string {
  return path === "" ? key : `${path}.${key}`;
}

function quote(text: string): string {
  return JSON.stringify(text);
}

function checkDuplicates(value: Json, path: string, issues: PolicyIssue[]): void {
  if (value.type === "object") {
    const seen = new Set<string>();
    for (const [key, child] of value.entries) {
      if (seen.has(key)) {
        issues.push({ path, message: `duplicate element ${quote(key)}` });
      }
      seen.add(key);
      checkDuplicates(child, childPath(path, key), issues);
    }
  } else if (value.type === "array") {
    value.items.forEach((item, index) => checkDuplicates(item, `${path}[${index}]`, issues));
  }
}

function checkStringList(value: Json, path: string, issues: PolicyIssue[]): void {
  if (value.type === "string") {
    if (value.value.length === 0) {
      issues.push({ path, message: "must not be empty" });
    }
  } else if (value.type === "array") {
    if (value.items.length === 0) {
      issues.push({ path, message: "must not be an empty list" });
    }
    value.items.forEach((item, index) => {
      if (item.type !== "string") {
        issues.push({ path: `${path}[${index}]`, message: `must be a string, not ${describe(item)}` });
      } else if (item.value.length === 0) {
        issues.push({ path: `${path}[${index}]`, message: "must not be empty" });
      }
    });
  } else {
    issues.push({ path, message: `must be a string or a list of strings, not ${describe(value)}` });
  }
}

function checkPrincipal(value: Json, path: string, issues: PolicyIssue[]): void {
  if (value.type === "string") {
    if (value.value !== "*") {
      issues.push({
        path,
        message: `must be "*" or an object such as {"AWS": "arn:aws:iam::123456789012:root"} (found ${quote(value.value)})`,
      });
    }
  } else if (value.type === "object") {
    if (value.entries.length === 0) {
      issues.push({ path, message: "must not be empty" });
    }
    for (const [kind, principals] of value.entries) {
      checkStringList(principals, childPath(path, kind), issues);
    }
  } else {
    issues.push({ path, message: `must be "*" or an object, not ${describe(value)}` });
  }
}

function isScalar(value: Json): boolean {
  return value.type === "string" || value.type === "number" || value.type === "bool";
}

function checkCondition(value: Json, path: string, issues: PolicyIssue[]): void {
  if (value.type !== "object") {
    issues.push({
      path,
      message: `must be an object that maps condition operators to condition keys, not ${describe(value)}`,
    });
    return;
  }
  for (const [operator, block] of value.entries) {
    const operatorPath = childPath(path, operator);
    if (block.type !== "object") {
      issues.push({
        path: operatorPath,
        message: `must be an object that maps condition keys to values, not ${describe(block)}`,
      });
      continue;
    }
    for (const [key, conditionValue] of block.entries) {
      const keyPath = childPath(operatorPath, key);
      if (isScalar(conditionValue)) {
        continue;
      }
      if (conditionValue.type === "array") {
        if (conditionValue.items.length === 0) {
          issues.push({ path: keyPath, message: "must not be an empty list" });
        }
        conditionValue.items.forEach((item, index) => {
          if (!isScalar(item)) {
            issues.push({
              path: `${keyPath}[${index}]`,
              message: `must be a string, number or boolean, not ${describe(item)}`,
            });
          }
        });
      } else {
        issues.push({
          path: keyPath,
          message: `must be a string, number, boolean or a list of them, not ${describe(conditionValue)}`,
        });
      }
    }
  }
}

function checkStatement(
  statement: Json,
  path: string,
  sids: Map<string, string>,
  issues: PolicyIssue[],
): void {
  if (statement.type !== "object") {
    issues.push({ path, message: `must be an object, not ${describe(statement)}` });
    return;
  }
  const entries = statement.entries;
  for (const [key] of entries) {
    if (!STATEMENT_ELEMENTS.includes(key)) {
      issues.push({
        path,
        message: `unknown element ${quote(key)}; a statement has ${STATEMENT_ELEMENTS.join(", ")}`,
      });
    }
  }
  const sid = first(entries, "Sid");
  if (sid !== undefined) {
    if (sid.type !== "string") {
      issues.push({ path: childPath(path, "Sid"), message: `must be a string, not ${describe(sid)}` });
    } else if (sids.has(sid.value)) {
      issues.push({
        path: childPath(path, "Sid"),
        message: `${quote(sid.value)} is already used by ${sids.get(sid.value) ?? ""}; Sid values must be unique`,
      });
    } else {
      sids.set(sid.value, path);
    }
  }
  const effect = first(entries, "Effect");
  if (effect === undefined) {
    issues.push({ path, message: 'missing "Effect"; use "Allow" or "Deny"' });
  } else if (effect.type !== "string") {
    issues.push({
      path: childPath(path, "Effect"),
      message: `must be "Allow" or "Deny", not ${describe(effect)}`,
    });
  } else if (effect.value !== "Allow" && effect.value !== "Deny") {
    issues.push({
      path: childPath(path, "Effect"),
      message: `must be "Allow" or "Deny" (found ${quote(effect.value)})`,
    });
  }
  for (const [plain, negated] of [
    ["Principal", "NotPrincipal"],
    ["Action", "NotAction"],
    ["Resource", "NotResource"],
  ] as const) {
    const hasPlain = first(entries, plain) !== undefined;
    const hasNegated = first(entries, negated) !== undefined;
    if (!hasPlain && !hasNegated) {
      issues.push({ path, message: `needs "${plain}" or "${negated}"` });
    } else if (hasPlain && hasNegated) {
      issues.push({ path, message: `cannot have both "${plain}" and "${negated}"` });
    }
  }
  for (const name of ["Principal", "NotPrincipal"]) {
    const value = first(entries, name);
    if (value !== undefined) {
      checkPrincipal(value, childPath(path, name), issues);
    }
  }
  for (const name of ["Action", "NotAction", "Resource", "NotResource"]) {
    const value = first(entries, name);
    if (value !== undefined) {
      checkStringList(value, childPath(path, name), issues);
    }
  }
  const condition = first(entries, "Condition");
  if (condition !== undefined) {
    checkCondition(condition, childPath(path, "Condition"), issues);
  }
}

function checkDocument(document: Json, issues: PolicyIssue[]): void {
  if (document.type !== "object") {
    issues.push({ path: "", message: `the policy must be a JSON object, not ${describe(document)}` });
    return;
  }
  const entries = document.entries;
  checkDuplicates(document, "", issues);
  for (const [key] of entries) {
    if (!POLICY_ELEMENTS.includes(key)) {
      issues.push({ path: "", message: `unknown element ${quote(key)}; a policy has Version, Id and Statement` });
    }
  }
  const version = first(entries, "Version");
  if (version === undefined) {
    issues.push({ path: "", message: 'missing "Version"; add "Version": "2012-10-17"' });
  } else if (version.type !== "string") {
    issues.push({ path: "Version", message: `must be a string, not ${describe(version)}` });
  } else if (!POLICY_VERSIONS.includes(version.value)) {
    issues.push({
      path: "Version",
      message: `must be "2012-10-17" or "2008-10-17" (found ${quote(version.value)})`,
    });
  }
  const id = first(entries, "Id");
  if (id !== undefined && id.type !== "string") {
    issues.push({ path: "Id", message: `must be a string, not ${describe(id)}` });
  }
  const sids = new Map<string, string>();
  const statement = first(entries, "Statement");
  if (statement === undefined) {
    issues.push({ path: "", message: 'missing "Statement"' });
  } else if (statement.type === "array") {
    if (statement.items.length === 0) {
      issues.push({ path: "Statement", message: "must contain at least one statement" });
    }
    statement.items.forEach((item, index) => checkStatement(item, `Statement[${index}]`, sids, issues));
  } else if (statement.type === "object") {
    // IAM allows one statement without the list around it.
    checkStatement(statement, "Statement", sids, issues);
  } else {
    issues.push({
      path: "Statement",
      message: `must be a list of statements (or one statement), not ${describe(statement)}`,
    });
  }
}

/** Every problem of a policy document, each with its path; an empty list means it is valid. */
export function checkPolicy(text: string): PolicyIssue[] {
  const parsed = parsePolicy(text);
  if ("issue" in parsed) {
    return [parsed.issue];
  }
  const issues: PolicyIssue[] = [];
  const size = utf8Length(serialize(parsed.document, null, 0));
  if (size > MAX_POLICY_BYTES) {
    issues.push({
      path: "",
      message: `the policy is ${size} bytes without whitespace; S3 accepts at most ${MAX_POLICY_BYTES} bytes (20 KiB)`,
    });
  }
  checkDocument(parsed.document, issues);
  return issues;
}

/** One issue as a line of text: the path, then the message. */
export function issueText(issue: PolicyIssue): string {
  return issue.path === "" ? issue.message : `${issue.path}: ${issue.message}`;
}

/** The text of a policy without whitespace, or the issues that make it invalid. */
export function compactPolicy(text: string): { text: string } | { issues: PolicyIssue[] } {
  const issues = checkPolicy(text);
  if (issues.length > 0) {
    return { issues };
  }
  const parsed = parsePolicy(text);
  return "document" in parsed ? { text: serialize(parsed.document, null, 0) } : { issues: [parsed.issue] };
}

/**
 * Pretty-prints a policy with two-space indentation and the keys in the order
 * written. Only the JSON syntax has to be right.
 */
export function formatPolicy(text: string): { text: string } | { issue: PolicyIssue } {
  const parsed = parsePolicy(text);
  return "issue" in parsed ? parsed : { text: serialize(parsed.document, "  ", 0) };
}
