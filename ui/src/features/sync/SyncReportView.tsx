import { Alert, Stack, Table, Text } from "@mantine/core";
import { IconChecklist } from "@tabler/icons-react";
import type { SyncKindTotals, SyncReport } from "../../api/types";
import { formatBytes, formatCount } from "../../lib/format";
import { formatRunDuration } from "../../lib/sync";

interface Line {
  label: string;
  totals: SyncKindTotals;
  bytes: boolean;
}

function lines(report: SyncReport): Line[] {
  const all: Line[] = [
    { label: "Uploaded", totals: report.upload, bytes: true },
    { label: "Downloaded", totals: report.download, bytes: true },
    { label: "Copied between prefixes", totals: report.copy, bytes: true },
    { label: "Deleted locally", totals: report.delete_local, bytes: false },
    { label: "Deleted in S3", totals: report.delete_remote, bytes: false },
    { label: "Already in sync", totals: report.equal, bytes: false },
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
      title={report.cancelled ? "Summary (cancelled)" : "Summary"}
      py="xs"
      aria-label="Sync summary"
    >
      <Stack gap={4}>
        <Table withRowBorders={false} verticalSpacing={2} horizontalSpacing="xs">
          <Table.Tbody>
            {shown.map(({ label, totals, bytes }) => (
              <Table.Tr key={label}>
                <Table.Td>{label}</Table.Td>
                <Table.Td>
                  {formatCount(totals.files)} {totals.files === 1 ? "file" : "files"}
                  {bytes ? `, ${formatBytes(totals.bytes)}` : ""}
                  {totals.failed > 0 ? `, ${formatCount(totals.failed)} failed` : ""}
                  {totals.pending > 0 ? `, ${formatCount(totals.pending)} not done` : ""}
                </Table.Td>
              </Table.Tr>
            ))}
            {report.unsyncable > 0 ? (
              <Table.Tr>
                <Table.Td>Could not be planned</Table.Td>
                <Table.Td>{formatCount(report.unsyncable)}</Table.Td>
              </Table.Tr>
            ) : null}
            <Table.Tr>
              <Table.Td>Duration</Table.Td>
              <Table.Td>{formatRunDuration(report.duration_ms)}</Table.Td>
            </Table.Tr>
            <Table.Tr>
              <Table.Td>Failures</Table.Td>
              <Table.Td>{formatCount(report.failed)}</Table.Td>
            </Table.Tr>
          </Table.Tbody>
        </Table>
        {report.failures.length > 0 ? (
          <Text size="xs" c="dimmed">
            {report.failures.length < report.failed
              ? `The first ${formatCount(report.failures.length)} failures are listed in the table below with their reasons.`
              : "Failed rows are listed in the table below with their reasons."}
          </Text>
        ) : null}
      </Stack>
    </Alert>
  );
}
