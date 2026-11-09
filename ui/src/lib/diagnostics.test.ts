import { describe, expect, it } from "vitest";
import type { DiagnosticsReport } from "../api/types";
import { tr } from "../i18n";
import { failedSteps, formatMillis, formatReport, STEP_LABELS, STATUS_LABELS, verdict } from "./diagnostics";

function report(overrides: Partial<DiagnosticsReport> = {}): DiagnosticsReport {
  return {
    target: "https://s3.eu-west-1.amazonaws.com",
    region: "eu-west-1",
    bucket: "demo-photos",
    started_at: "2026-10-29T08:15:00Z",
    elapsed_ms: 412,
    ok: true,
    steps: [
      {
        id: "dns",
        status: "pass",
        elapsed_ms: 12,
        summary: "s3.eu-west-1.amazonaws.com resolves to 2 addresses.",
        detail: "192.0.2.10, 198.51.100.7",
        code: "resolved",
      },
      { id: "tcp", status: "pass", elapsed_ms: 31, summary: "Connected to 192.0.2.10:443.", detail: null, code: "connected" },
      { id: "proxy", status: "skip", elapsed_ms: 0, summary: "No proxy is configured.", detail: null, code: "none" },
      {
        id: "tls",
        status: "pass",
        elapsed_ms: 58,
        summary: "TLS 1.3 handshake succeeded.",
        detail: "Certificate for CN=*.s3.example.com\nIssued by CN=Example CA\nExpires 2027-03-01",
        code: "handshake",
      },
      { id: "credentials", status: "pass", elapsed_ms: 1210, summary: "The server accepted the credentials.", detail: null, code: "valid" },
      { id: "region", status: "pass", elapsed_ms: 95, summary: "Bucket demo-photos is in eu-west-1.", detail: null, code: "found" },
    ],
    ...overrides,
  };
}

describe("formatMillis", () => {
  it("uses milliseconds below a second and seconds above", () => {
    expect(formatMillis(0)).toBe("0 ms");
    expect(formatMillis(12)).toBe("12 ms");
    expect(formatMillis(999.4)).toBe("999 ms");
    expect(formatMillis(1000)).toBe("1.0 s");
    expect(formatMillis(1210)).toBe("1.2 s");
    expect(formatMillis(-1)).toBe("");
    expect(formatMillis(Number.NaN)).toBe("");
  });
});

describe("verdict", () => {
  it("says all checks passed, or names the failed steps", () => {
    expect(verdict(report())).toBe(tr("All checks passed."));
    const failing = report({
      ok: false,
      steps: report().steps.map((step) =>
        step.id === "credentials" || step.id === "region" ? { ...step, status: "fail" as const } : step,
      ),
    });
    expect(failedSteps(failing).map((step) => step.id)).toEqual(["credentials", "region"]);
    expect(verdict(failing)).toBe(
      tr("{n} checks failed: {names}.", { n: 2, names: `${tr("Credentials")}, ${tr("Bucket region")}` }),
    );
    const one = report({ steps: report().steps.map((step) => (step.id === "dns" ? { ...step, status: "fail" as const } : step)) });
    expect(verdict(one)).toBe(tr("1 check failed: {names}.", { names: tr("DNS lookup") }));
  });
});

describe("formatReport", () => {
  it("produces the copyable text", () => {
    const nameWidth = Math.max(...Object.values(STEP_LABELS).map((label) => label.length));
    const codeWidth = Math.max(0, ...report().steps.map((step) => step.code.length));
    const line = (status: "pass" | "fail" | "skip", id: "dns" | "tcp" | "proxy" | "tls" | "credentials" | "region", time: string, code: string, summary: string) =>
      `[${STATUS_LABELS[status].toUpperCase()}] ${STEP_LABELS[id].padEnd(nameWidth)}  ${time.padStart(7)}  ${code.padEnd(codeWidth)}  ${summary}`;
    expect(formatReport(report())).toBe(
      [
        tr("BucketBridge diagnostics"),
        tr("Endpoint: {target}", { target: "https://s3.eu-west-1.amazonaws.com" }),
        tr("Region: {region}", { region: "eu-west-1" }),
        tr("Bucket: {bucket}", { bucket: "demo-photos" }),
        tr("Started: {start} ({elapsed} in total)", { start: "2026-10-29T08:15:00Z", elapsed: "412 ms" }),
        tr("Result: {verdict}", { verdict: tr("All checks passed.") }),
        "",
        line("pass", "dns", "12 ms", "resolved", "s3.eu-west-1.amazonaws.com resolves to 2 addresses."),
        "      192.0.2.10, 198.51.100.7",
        line("pass", "tcp", "31 ms", "connected", "Connected to 192.0.2.10:443."),
        line("skip", "proxy", "-", "none", "No proxy is configured."),
        line("pass", "tls", "58 ms", "handshake", "TLS 1.3 handshake succeeded."),
        "      Certificate for CN=*.s3.example.com",
        "      Issued by CN=Example CA",
        "      Expires 2027-03-01",
        line("pass", "credentials", "1.2 s", "valid", "The server accepted the credentials."),
        line("pass", "region", "95 ms", "found", "Bucket demo-photos is in eu-west-1."),
        "",
      ].join("\n"),
    );
  });

  it("names a missing bucket and a failed run, and never contains an em dash", () => {
    const failing = report({
      bucket: null,
      ok: false,
      steps: report().steps.map((step) =>
        step.id === "credentials"
          ? { ...step, status: "fail" as const, code: "invalid_access_key", summary: "The access key ID does not exist." }
          : step,
      ),
    });
    const text = formatReport(failing);
    expect(text).toContain(tr("Bucket: {bucket}", { bucket: tr("(none)") }));
    expect(text).toContain(tr("1 check failed: {names}.", { names: tr("Credentials") }));
    expect(text).toContain(`[${STATUS_LABELS.fail.toUpperCase()}] ${tr("Credentials")}`);
    expect(text).toContain("invalid_access_key");
    expect(text.includes(String.fromCodePoint(0x2014))).toBe(false);
  });
});
