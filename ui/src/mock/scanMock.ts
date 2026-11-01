// The security scan of the mock backend (docs/desktop-api.md, "Security scan (T13)"): findings follow
// the bucket name so that tests and the browser demo see every severity.

import type { BucketScan, ScanFinding, ScanReport } from "../api/types";

const FINDINGS: Readonly<Record<string, ScanFinding>> = {
  public_acl: {
    id: "public_acl",
    severity: "critical",
    title: "The bucket ACL grants public access",
    explanation: "The ACL lets everyone write objects or change the ACL. Remove the grant and turn on the public access block.",
    fix_tab: "public-access",
  },
  public_policy: {
    id: "public_policy",
    severity: "critical",
    title: "The bucket policy makes the bucket public",
    explanation: "A statement of the bucket policy allows everyone without a condition. Restrict the principal.",
    fix_tab: "policy",
  },
  public_access_block_off: {
    id: "public_access_block_off",
    severity: "high",
    title: "The public access block is off",
    explanation: "Nothing stops a public ACL or policy from being added. Turn on all four toggles.",
    fix_tab: "public-access",
  },
  no_default_encryption: {
    id: "no_default_encryption",
    severity: "medium",
    title: "No default encryption",
    explanation: "Objects written without an encryption header are stored unencrypted.",
    fix_tab: "encryption",
  },
  versioning_off: {
    id: "versioning_off",
    severity: "low",
    title: "Versioning is not enabled",
    explanation: "An overwritten or deleted object cannot be recovered. Enable versioning.",
    fix_tab: "versioning",
  },
};

/** Findings of one mock bucket: `*public*` is open, `*plain*` is unprotected, anything else only lacks versioning. */
export function scanBucket(name: string): BucketScan {
  const pick = (...ids: string[]): ScanFinding[] => ids.flatMap((id) => (FINDINGS[id] === undefined ? [] : [{ ...FINDINGS[id] }]));
  let findings: ScanFinding[];
  if (name.includes("public")) {
    findings = pick("public_acl", "public_policy", "public_access_block_off", "no_default_encryption", "versioning_off");
  } else if (name.includes("plain")) {
    findings = pick("public_access_block_off", "no_default_encryption", "versioning_off");
  } else {
    findings = pick("versioning_off");
  }
  return { bucket: name, findings, skipped: [], error: null };
}

export function scanReport(names: readonly string[], at: string): ScanReport {
  return { generated_at: at, buckets: [...new Set(names)].map(scanBucket) };
}

/** The size of a report as the backend would write it. */
export function reportBytes(report: ScanReport, format: "csv" | "json"): number {
  const text = format === "json" ? JSON.stringify(report, null, 2) : report.buckets.flatMap((b) => b.findings.map((f) => `${b.bucket},${f.severity},${f.title}`)).join("\n");
  return new TextEncoder().encode(text).length;
}
