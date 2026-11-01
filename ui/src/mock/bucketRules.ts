// The bucket rules of the mock cloud: lifecycle, Intelligent-Tiering, CORS and replication, behaving like the
// real commands (docs/desktop-api.md, "Bucket rules (T04/T07)"): "nothing configured" is an answer, and invalid
// documents are rejected with `usage` errors that name the path of every problem. The mock checks the main rules;
// the Rust core checks all of them.

import { BackendError } from "../api/errors";
import type { BucketRules, CorsConfig, LifecycleConfig, ReplicationConfig, RuleIssue, TieringConfig } from "../api/types";
import { clone } from "./util";

function isSet(value: unknown): boolean {
  return value !== null && value !== undefined;
}

const METHODS = ["GET", "PUT", "POST", "DELETE", "HEAD"];

type Kind = "lifecycle" | "cors" | "replication";

function issue(path: string, message: string): RuleIssue {
  return { path, message };
}

/** Parses a document; a document that is not JSON is a single problem without a path. */
function parse(text: string): { value: Record<string, unknown> } | { issues: RuleIssue[] } {
  if (text.trim() === "") {
    return { issues: [issue("", "the document is empty")] };
  }
  try {
    const value: unknown = JSON.parse(text);
    if (typeof value !== "object" || value === null || Array.isArray(value)) {
      return { issues: [issue("", "not valid: the document must be a JSON object")] };
    }
    return { value: value as Record<string, unknown> };
  } catch (error) {
    return { issues: [issue("", `not valid: ${error instanceof Error ? error.message : String(error)}`)] };
  }
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

export function lifecycleIssues(text: string): RuleIssue[] {
  const parsed = parse(text);
  if ("issues" in parsed) {
    return parsed.issues;
  }
  const rules = Array.isArray(parsed.value.rules) ? (parsed.value.rules as Record<string, unknown>[]) : [];
  if (rules.length === 0) {
    return [issue("rules", "add at least one rule; to remove every rule, delete the configuration")];
  }
  const issues: RuleIssue[] = [];
  rules.forEach((rule, index) => {
    const path = `rules[${index}]`;
    const transitions = Array.isArray(rule.transitions) ? (rule.transitions as Record<string, unknown>[]) : [];
    const hasAction =
      transitions.length > 0 ||
      isSet(rule.expiration) ||
      (Array.isArray(rule.noncurrent_version_transitions) && rule.noncurrent_version_transitions.length > 0) ||
      isSet(rule.noncurrent_version_expiration) ||
      isSet(rule.abort_incomplete_multipart_upload_days);
    if (!hasAction) {
      issues.push(issue(path, "has no action; add a transition, an expiration, a noncurrent version action or an abort of incomplete multipart uploads"));
    }
    transitions.forEach((transition, n) => {
      const at = `${path}.transitions[${n}]`;
      if (isSet(transition.days) && isSet(transition.date)) {
        issues.push(issue(at, "set either days or date, not both"));
      } else if (!isSet(transition.days) && !isSet(transition.date)) {
        issues.push(issue(at, "set days or date"));
      } else if (
        typeof transition.days === "number" &&
        (transition.storage_class === "STANDARD_IA" || transition.storage_class === "ONEZONE_IA") &&
        transition.days < 30
      ) {
        issues.push(issue(`${at}.days`, `objects stay at least 30 days in STANDARD before they move to ${String(transition.storage_class)}`));
      }
    });
    const expiration = rule.expiration as Record<string, unknown> | null | undefined;
    if (expiration !== null && expiration !== undefined) {
      const set = [isSet(expiration.days), isSet(expiration.date), expiration.expired_object_delete_marker === true].filter(Boolean).length;
      if (set !== 1) {
        issues.push(issue(`${path}.expiration`, set === 0 ? "set days, date or expired_object_delete_marker" : "set only one of days, date and expired_object_delete_marker"));
      }
    }
  });
  return issues;
}

export function corsIssues(text: string): RuleIssue[] {
  const parsed = parse(text);
  if ("issues" in parsed) {
    return parsed.issues;
  }
  const rules = Array.isArray(parsed.value.rules) ? (parsed.value.rules as Record<string, unknown>[]) : [];
  if (rules.length === 0) {
    return [issue("rules", "add at least one rule; to remove every rule, delete the configuration")];
  }
  const issues: RuleIssue[] = [];
  rules.forEach((rule, index) => {
    const path = `rules[${index}]`;
    if (strings(rule.allowed_origins).length === 0) {
      issues.push(issue(`${path}.allowed_origins`, "allow at least one origin (use * for any)"));
    }
    const methods = strings(rule.allowed_methods);
    if (methods.length === 0) {
      issues.push(issue(`${path}.allowed_methods`, `allow at least one method of ${METHODS.join(", ")}`));
    }
    methods.forEach((method, n) => {
      if (!METHODS.includes(method)) {
        issues.push(issue(`${path}.allowed_methods[${n}]`, `"${method}" is not allowed; use one of ${METHODS.join(", ")} (upper case)`));
      }
    });
  });
  return issues;
}

export function tieringIssues(text: string): RuleIssue[] {
  const parsed = parse(text);
  if ("issues" in parsed) {
    return parsed.issues;
  }
  const issues: RuleIssue[] = [];
  const id = parsed.value.id;
  if (typeof id !== "string" || id.length === 0 || id.length > 64) {
    issues.push(issue("id", "must be 1 to 64 characters"));
  }
  const steps = Array.isArray(parsed.value.tierings) ? (parsed.value.tierings as Record<string, unknown>[]) : [];
  if (steps.length === 0) {
    issues.push(issue("tierings", "add at least one archive tier (ARCHIVE_ACCESS or DEEP_ARCHIVE_ACCESS)"));
  }
  steps.forEach((step, index) => {
    const [low, high] = step.access_tier === "DEEP_ARCHIVE_ACCESS" ? [180, 730] : [90, 730];
    if (typeof step.days !== "number" || step.days < low || step.days > high) {
      issues.push(issue(`tierings[${index}].days`, `${String(step.access_tier)} takes ${low} to ${high} days`));
    }
  });
  return issues;
}

export function replicationIssues(text: string): RuleIssue[] {
  const parsed = parse(text);
  if ("issues" in parsed) {
    return parsed.issues;
  }
  const issues: RuleIssue[] = [];
  const role = parsed.value.role;
  if (typeof role !== "string" || role === "") {
    issues.push(issue("role", "is required: the ARN of the IAM role S3 uses to replicate (arn:aws:iam::<account>:role/<name>)"));
  }
  const rules = Array.isArray(parsed.value.rules) ? (parsed.value.rules as Record<string, unknown>[]) : [];
  if (rules.length === 0) {
    issues.push(issue("rules", "add at least one rule; to stop replicating, delete the configuration"));
  }
  rules.forEach((rule, index) => {
    if (typeof rule.destination_bucket !== "string" || !rule.destination_bucket.startsWith("arn:aws:s3:::")) {
      issues.push(issue(`rules[${index}].destination_bucket`, "is not a bucket ARN; use arn:aws:s3:::<bucket>"));
    }
  });
  return issues;
}

function usage(issues: RuleIssue[], what: string): never {
  const text = issues.map((entry) => (entry.path === "" ? entry.message : `${entry.path}: ${entry.message}`)).join("; ");
  throw new BackendError("usage", `invalid ${what}: ${text}`);
}

/** The rule settings of every bucket, keyed by the bucket's slot key. */
export class MockBucketRules {
  private readonly docs = new Map<string, unknown>();

  private get<T>(key: string, kind: Kind | `tiering/${string}`): T | null {
    const value = this.docs.get(`${key}:${kind}`);
    return value === undefined ? null : clone(value as T);
  }

  private put(key: string, kind: Kind | `tiering/${string}`, value: unknown): void {
    this.docs.set(`${key}:${kind}`, clone(value));
  }

  rules(key: string): BucketRules {
    return {
      cors: { type: "loaded", value: this.get<CorsConfig>(key, "cors") },
      lifecycle: { type: "loaded", value: this.get<LifecycleConfig>(key, "lifecycle") },
      tiering: { type: "loaded", value: this.tiering(key) },
      replication: { type: "loaded", value: this.get<ReplicationConfig>(key, "replication") },
    };
  }

  getLifecycle(key: string): LifecycleConfig | null {
    return this.get(key, "lifecycle");
  }
  putLifecycle(key: string, config: LifecycleConfig): void {
    const issues = lifecycleIssues(JSON.stringify(config));
    if (issues.length > 0) {
      usage(issues, "lifecycle configuration");
    }
    this.put(key, "lifecycle", config);
  }
  deleteLifecycle(key: string): void {
    this.docs.delete(`${key}:lifecycle`);
  }

  getCors(key: string): CorsConfig | null {
    return this.get(key, "cors");
  }
  putCors(key: string, config: CorsConfig): void {
    const issues = corsIssues(JSON.stringify(config));
    if (issues.length > 0) {
      usage(issues, "CORS configuration");
    }
    this.put(key, "cors", config);
  }
  deleteCors(key: string): void {
    this.docs.delete(`${key}:cors`);
  }

  getReplication(key: string): ReplicationConfig | null {
    return this.get(key, "replication");
  }
  putReplication(key: string, bucket: string, versioningEnabled: boolean, config: ReplicationConfig): void {
    const issues = replicationIssues(JSON.stringify(config));
    if (issues.length > 0) {
      usage(issues, "replication configuration");
    }
    if (!versioningEnabled) {
      throw new BackendError(
        "usage",
        `replication needs versioning on the source bucket, and versioning of '${bucket}' is not enabled; enable versioning in the bucket properties first`,
      );
    }
    this.put(key, "replication", config);
  }
  deleteReplication(key: string): void {
    this.docs.delete(`${key}:replication`);
  }

  tiering(key: string): TieringConfig[] {
    const prefix = `${key}:tiering/`;
    return [...this.docs.entries()]
      .filter(([name]) => name.startsWith(prefix))
      .map(([, value]) => clone(value as TieringConfig))
      .sort((a, b) => (a.id < b.id ? -1 : 1));
  }
  putTiering(key: string, config: TieringConfig): void {
    const issues = tieringIssues(JSON.stringify(config));
    if (issues.length > 0) {
      usage(issues, "Intelligent-Tiering configuration");
    }
    this.put(key, `tiering/${config.id}`, config);
  }
  deleteTiering(key: string, id: string): void {
    this.docs.delete(`${key}:tiering/${id}`);
  }
}
