import { Alert, Stack, Table, Text } from "@mantine/core";
import { IconChecklist } from "@tabler/icons-react";
import type { SyncKindTotals, SyncReport } from "../../api/types";
import { formatBytes, formatCount } from "../../lib/format";
import { formatRunDuration } from "../../lib/sync";
import { tr } from "../../i18n";

interface Line {
  label: string;
  totals: SyncKindTotals;
  bytes: boolean;
}

function lines(report: SyncReport): Line[] {
  const all: Line[] = [
    { label: tr("Uploaded"), totals: report.upload, bytes: true },
    { label: tr("Downloaded"), totals: report.download, bytes: true },
    { label: tr("Copied between prefixes"), totals: report.copy, bytes: true },
    { label: tr("Deleted locally"), totals: report.delete_local, bytes: false },
    { label: tr("Deleted in S3"), totals: report.delete_remote, bytes: false },
    { label: tr("Already in sync"), totals: report.equal, bytes: false },
  ];
  return all.filter(({ totals }) => totals.files + totals.failed + totals.pending > 0);
}

/** The summary report shown when a run ends: counts and bytes per action, the duration and the failures. */
export function SyncReportView({ report }: { report: SyncReport }) {
  const shown = lines(report);
  return (
    <Alert
      color={report.failed > 0 ? "orange" : "teal"}
      icon={<IconChecklist size={16} />}
      title={report.cancelled ? tr("Summary (cancelled)") : tr("Summary")}
      py="xs"
      aria-label={tr("Sync summary")}
    >
      <Stack gap={4}>
        <Table withRowBorders={false} verticalSpacing={2} horizontalSpacing="xs">
          <Table.Tbody>
            {shown.map(({ label, totals, bytes }) => (
              <Table.Tr key={label}>
                <Table.Td>{label}</Table.Td>
                <Table.Td>
                  {formatCount(totals.files)} {totals.files === 1 ? tr("file") : tr("files")}
                  {bytes ? tr(", {bytes}", { bytes: formatBytes(totals.bytes) }) : ""}
                  {totals.failed > 0 ? tr(", {n} failed", { n: formatCount(totals.failed) }) : ""}
                  {totals.pending > 0 ? tr(", {n} not done", { n: formatCount(totals.pending) }) : ""}
                </Table.Td>
              </Table.Tr>
            ))}
            {report.unsyncable > 0 ? (
              <Table.Tr>
                <Table.Td>{tr("Could not be planned")}</Table.Td>
                <Table.Td>{formatCount(report.unsyncable)}</Table.Td>
              </Table.Tr>
            ) : null}
            <Table.Tr>
              <Table.Td>{tr("Duration")}</Table.Td>
              <Table.Td>{formatRunDuration(report.duration_ms)}</Table.Td>
            </Table.Tr>
            <Table.Tr>
              <Table.Td>{tr("Failures")}</Table.Td>
              <Table.Td>{formatCount(report.failed)}</Table.Td>
            </Table.Tr>
          </Table.Tbody>
        </Table>
        {report.failures.length > 0 ? (
          <Text size="xs" c="dimmed">
            {report.failures.length < report.failed
              ? tr("The first {n} failures are listed in the table below with their reasons.", {
                  n: formatCount(report.failures.length),
                })
              : tr("Failed rows are listed in the table below with their reasons.")}
          </Text>
        ) : null}
      </Stack>
    </Alert>
  );
}
