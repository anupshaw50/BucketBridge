import type { BucketPropertiesTab } from "../state/dialogs";
import type { BucketScan, ScanFinding, ScanFixTab, ScanReport, ScanSeverity } from "../api/types";

export const SEVERITY_LABELS: Readonly<Record<ScanSeverity, string>> = {
  critical: "Critical",
  high: "High",
  medium: "Medium",
  low: "Low",
};

const RANK: Readonly<Record<ScanSeverity, number>> = { critical: 3, high: 2, medium: 1, low: 0 };

export const FIX_TAB_LABELS: Readonly<Record<ScanFixTab, string>> = {
  policy: "Policy",
  "public-access": "Public access",
  versioning: "Versioning",
  encryption: "Encryption",
  ownership: "Ownership",
};

/** The Properties tab of the dialog for a fix, or null when this build has no editor for that setting. */
export function openableTab(tab: ScanFixTab): BucketPropertiesTab | null {
  return tab === "policy" || tab === "public-access" || tab === "versioning" ? tab : null;
}

export interface ScanRow {
  bucket: string;
  finding: ScanFinding;
}

/** Every finding, most severe first, then by bucket name. */
export function scanRows(report: ScanReport): ScanRow[] {
  return report.buckets
    .flatMap((scan: BucketScan) => scan.findings.map((finding) => ({ bucket: scan.bucket, finding })))
    .sort((a, b) => RANK[b.finding.severity] - RANK[a.finding.severity] || a.bucket.localeCompare(b.bucket));
}

export function countBySeverity(report: ScanReport, severity: ScanSeverity): number {
  return report.buckets.reduce((sum, scan) => sum + scan.findings.filter((f) => f.severity === severity).length, 0);
}

export function reportFileName(format: "csv" | "json"): string {
  return `security-scan.${format}`;
}
