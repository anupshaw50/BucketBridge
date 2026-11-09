import type { DiagnosticStatus, DiagnosticStep, DiagnosticStepId, DiagnosticsReport } from "../api/types";
import { tr } from "../i18n";

/** Display names of the diagnostic steps, in report order. */
export const STEP_LABELS: Record<DiagnosticStepId, string> = {
  dns: tr("DNS lookup"),
  tcp: tr("TCP connection"),
  proxy: tr("Proxy"),
  tls: tr("TLS handshake"),
  credentials: tr("Credentials"),
  region: tr("Bucket region"),
};

export const STATUS_LABELS: Record<DiagnosticStatus, string> = {
  pass: tr("Pass"),
  fail: tr("Fail"),
  skip: tr("Skip"),
};

/** The steps that failed, in report order. */
export function failedSteps(report: DiagnosticsReport): DiagnosticStep[] {
  return report.steps.filter((step) => step.status === "fail");
}

/** One sentence about the whole run: "All checks passed." or "2 checks failed: DNS lookup, Credentials." */
export function verdict(report: DiagnosticsReport): string {
  const failed = failedSteps(report);
  if (failed.length === 0) {
    return tr("All checks passed.");
  }
  const names = failed.map((step) => STEP_LABELS[step.id]).join(", ");
  return failed.length === 1
    ? tr("1 check failed: {names}.", { names })
    : tr("{n} checks failed: {names}.", { n: failed.length, names });
}

/** "12 ms" below a second, "1.4 s" above. */
export function formatMillis(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) {
    return "";
  }
  return ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`;
}

/**
 * The plain-text report that "Copy report" puts on the clipboard: a header, one aligned line per
 * step (status, name, time, outcome code, summary) and the detail of a step below it, indented.
 * It carries what the backend reported and nothing else, so it holds no secret.
 */
export function formatReport(report: DiagnosticsReport): string {
  const lines = [
    tr("BucketBridge diagnostics"),
    tr("Endpoint: {target}", { target: report.target }),
    tr("Region: {region}", { region: report.region }),
    tr("Bucket: {bucket}", { bucket: report.bucket ?? tr("(none)") }),
    tr("Started: {start} ({elapsed} in total)", { start: report.started_at, elapsed: formatMillis(report.elapsed_ms) }),
    tr("Result: {verdict}", { verdict: verdict(report) }),
    "",
  ];
  const nameWidth = Math.max(...Object.values(STEP_LABELS).map((label) => label.length));
  const codeWidth = Math.max(0, ...report.steps.map((step) => step.code.length));
  for (const step of report.steps) {
    const time = step.status === "skip" ? "-" : formatMillis(step.elapsed_ms);
    lines.push(
      `[${STATUS_LABELS[step.status].toUpperCase()}] ${STEP_LABELS[step.id].padEnd(nameWidth)}  ${time.padStart(7)}  ${step.code.padEnd(codeWidth)}  ${step.summary}`,
    );
    for (const detail of (step.detail ?? "").split("\n")) {
      if (detail.trim().length > 0) {
        lines.push(`      ${detail}`);
      }
    }
  }
  return `${lines.join("\n")}\n`;
}
