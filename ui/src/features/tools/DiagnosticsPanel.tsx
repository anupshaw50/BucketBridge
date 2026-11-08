import { Alert, Badge, Button, Group, Loader, Stack, Text } from "@mantine/core";
import { IconCheck, IconCircleCheck, IconCircleMinus, IconCircleX, IconCopy, IconPlayerPlay } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { AccountInput, DiagnosticStatus, DiagnosticStep, DiagnosticsReport } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { formatMillis, formatReport, STATUS_LABELS, STEP_LABELS, verdict } from "../../lib/diagnostics";

/** What a diagnostics run probes: a stored account, or an account the user is still typing (never stored). */
export type DiagnosticsTarget = { kind: "account"; accountId: string } | { kind: "input"; input: AccountInput };

export interface DiagnosticsPanelProps {
  target: DiagnosticsTarget;
  /** Bucket of the region probe, or `null` to skip that step. */
  bucket: string | null;
  /** Label of the button that starts the run. */
  runLabel?: string;
  disabled?: boolean;
}

const STATUS_ICONS: Record<DiagnosticStatus, React.ReactNode> = {
  pass: <IconCircleCheck size={18} color="var(--mantine-color-teal-6)" aria-hidden />,
  fail: <IconCircleX size={18} color="var(--mantine-color-red-6)" aria-hidden />,
  skip: <IconCircleMinus size={18} color="var(--mantine-color-gray-6)" aria-hidden />,
};

const STATUS_COLORS: Record<DiagnosticStatus, string> = { pass: "teal", fail: "red", skip: "gray" };

function StepRow({ step }: { step: DiagnosticStep }) {
  return (
    <li className="bb-diag-step" data-status={step.status}>
      {STATUS_ICONS[step.status]}
      <div style={{ minWidth: 0 }}>
        <Group gap="xs" wrap="nowrap">
          <Text size="sm" fw={600}>
            {STEP_LABELS[step.id]}
          </Text>
          <Badge size="xs" variant="light" color={STATUS_COLORS[step.status]}>
            {STATUS_LABELS[step.status]}
          </Badge>
        </Group>
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {step.summary}
        </Text>
        {step.detail !== null ? (
          <Text size="xs" c="dimmed" className="bb-diag-detail">
            {step.detail}
          </Text>
        ) : null}
      </div>
      <Stack gap={0} align="flex-end">
        <Text size="xs" c="dimmed" style={{ whiteSpace: "nowrap" }}>
          {step.status === "skip" ? tr("skipped") : formatMillis(step.elapsed_ms)}
        </Text>
        <Text size="xs" c="dimmed" ff="monospace">
          {step.code}
        </Text>
      </Stack>
    </li>
  );
}

/**
 * Runs the diagnostics of one account step by step and shows the report: a verdict, one row per
 * step with its timing and explanation, and a button that copies the report as text.
 */
export function DiagnosticsPanel({ target, bucket, runLabel = tr("Run diagnostics"), disabled = false }: DiagnosticsPanelProps) {
  const model = useModel();
  const [running, setRunning] = useState(false);
  const [report, setReport] = useState<DiagnosticsReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) {
      return undefined;
    }
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);

  const run = async () => {
    setRunning(true);
    setError(null);
    try {
      const next = await model.api.run_diagnostics({
        account_id: target.kind === "account" ? target.accountId : null,
        input: target.kind === "input" ? target.input : null,
        bucket,
      });
      setReport(next);
    } catch (raw) {
      setReport(null);
      setError(toBbError(raw).message);
    } finally {
      setRunning(false);
    }
  };

  const copy = async () => {
    if (report === null) {
      return;
    }
    try {
      await model.platform.copyText(formatReport(report));
      setCopied(true);
      model.status.info(tr("Diagnostics report copied to the clipboard."));
    } catch (raw) {
      model.status.error(raw, { notify: tr("The report could not be copied") });
    }
  };

  return (
    <Stack gap="sm">
      <Group justify="space-between" wrap="nowrap">
        <Button
          type="button"
          leftSection={<IconPlayerPlay size={14} />}
          loading={running}
          disabled={disabled}
          onClick={() => void run()}
        >
          {report === null ? runLabel : tr("Run again")}
        </Button>
        {report !== null ? (
          <Button
            type="button"
            variant="default"
            leftSection={copied ? <IconCheck size={14} /> : <IconCopy size={14} />}
            onClick={() => void copy()}
          >
            {copied ? tr("Copied") : tr("Copy report")}
          </Button>
        ) : null}
      </Group>
      {running ? (
        <Group gap="xs" aria-live="polite">
          <Loader size="sm" />
          <Text size="sm">{tr("Running diagnostics...")}</Text>
        </Group>
      ) : null}
      {error !== null ? (
        <Alert color="red" role="alert" py="xs">
          {error}
        </Alert>
      ) : null}
      {report !== null && !running ? (
        <Stack gap="xs">
          <Alert
            color={report.ok ? "teal" : "red"}
            icon={report.ok ? <IconCircleCheck size={16} /> : <IconCircleX size={16} />}
            title={verdict(report)}
            py="xs"
          >
            <Text size="sm" style={{ overflowWrap: "anywhere" }}>
              {tr("{target} ({region}{extras}), {time} in total.", {
                target: report.target,
                region: report.region,
                extras: report.bucket === null ? "" : tr(", bucket {bucket}", { bucket: report.bucket }),
                time: formatMillis(report.elapsed_ms),
              })}
            </Text>
          </Alert>
          <ol className="bb-diag-steps" aria-label={tr("Diagnostic steps")}>
            {report.steps.map((step) => (
              <StepRow key={step.id} step={step} />
            ))}
          </ol>
        </Stack>
      ) : null}
    </Stack>
  );
}
