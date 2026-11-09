import { Alert, Badge, Button, Group, MultiSelect, Stack, Table, Text } from "@mantine/core";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { ScanReport, ScanReportFormat, ScanSeverity } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { tr } from "../../i18n";
import { AppModal } from "../../components/AppModal";
import { countBySeverity, FIX_TAB_LABELS, openableTab, reportFileName, scanRows, SEVERITY_LABELS } from "../../lib/scan";

const COLORS: Readonly<Record<ScanSeverity, string>> = { critical: "red", high: "orange", medium: "yellow", low: "gray" };
const SEVERITIES: readonly ScanSeverity[] = ["critical", "high", "medium", "low"];

/**
 * Scans the chosen buckets of the active account (all of them when none is chosen) for public ACLs and policies,
 * a missing or partial public access block, missing default encryption, versioning that is not on and object
 * ownership that allows ACLs. The scan only reads; every finding links to the bucket Properties tab that fixes it.
 */
export function SecurityScanDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const accountId = useSlice(model.session.store, (state) => state.activeAccountId);
  const bucketList = useSlice(model.browser.store, (state) => state.bucketList);
  const names = bucketList.accountId === accountId ? bucketList.buckets.map((bucket) => bucket.name) : [];
  const [chosen, setChosen] = useState<string[]>([]);
  const [report, setReport] = useState<ScanReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const scan = async () => {
    if (accountId === null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const next = await model.api.run_security_scan({ account_id: accountId, buckets: chosen.length > 0 ? chosen : null });
      setReport(next);
      model.status.info(
        next.buckets.length === 1 ? tr("Security scan finished: 1 bucket checked.") : tr("Security scan finished: {n} buckets checked.", { n: next.buckets.length }),
      );
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  const save = async (format: ScanReportFormat) => {
    if (report === null) {
      return;
    }
    let path: string | null;
    try {
      path = await model.platform.pickSavePath({
        title: tr("Export security report"),
        defaultPath: reportFileName(format),
        filters: [{ name: format.toUpperCase(), extensions: [format] }],
      });
    } catch (raw) {
      model.status.error(raw, { notify: tr("The file dialog could not be opened") });
      return;
    }
    if (path === null) {
      return;
    }
    try {
      await model.api.export_scan_report({ report, format, path });
      model.status.info(tr("Security report saved to {path}.", { path }));
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  const fix = (bucket: string, tab: NonNullable<ReturnType<typeof openableTab>>) => {
    if (accountId !== null) {
      model.dialogs.open({ kind: "bucket-properties", accountId, bucket, tab });
    }
  };

  const rows = report === null ? [] : scanRows(report);
  const notes =
    report === null
      ? []
      : report.buckets.flatMap((item) => [
          ...item.skipped.map((skip) => `${item.bucket}: ${skip.message}`),
          ...(item.error === null ? [] : [`${item.bucket}: ${item.error.message}`]),
        ]);

  return (
    <AppModal opened onClose={onClose} title={tr("Security scan")} size="xl" closeOnClickOutside={false}>
      {accountId === null ? (
        <Stack gap="sm">
          <Text size="sm">{tr("Add or choose an account first.")}</Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              {tr("Close")}
            </Button>
          </Group>
        </Stack>
      ) : (
        <Stack gap="sm">
          <Text size="sm">{tr("Reads the access settings of your buckets and flags what is risky. Nothing is changed.")}</Text>
          <MultiSelect
            label={tr("Buckets")}
            description={tr("Leave empty to scan every bucket of the account.")}
            data={names}
            value={chosen}
            onChange={setChosen}
            searchable
            clearable
            comboboxProps={{ withinPortal: false }}
          />
          <Group>
            <Button onClick={() => void scan()} loading={busy}>
              {tr("Scan")}
            </Button>
            {report !== null && (
              <>
                <Button variant="default" onClick={() => void save("csv")}>
                  {tr("Export CSV")}
                </Button>
                <Button variant="default" onClick={() => void save("json")}>
                  {tr("Export JSON")}
                </Button>
              </>
            )}
          </Group>
          {error !== null && (
            <Alert color="red" role="alert">
              {error}
            </Alert>
          )}
          {report !== null && (
            <>
              <Group gap="xs" aria-label={tr("Summary")}>
                {SEVERITIES.map((severity) => (
                  <Badge key={severity} color={COLORS[severity]} variant="light">
                    {tr("{label}: {n}", { label: SEVERITY_LABELS[severity], n: countBySeverity(report, severity) })}
                  </Badge>
                ))}
              </Group>
              {rows.length === 0 ? (
                <Text size="sm">
                  {report.buckets.length === 1 ? tr("No findings in 1 bucket.") : tr("No findings in {n} buckets.", { n: report.buckets.length })}
                </Text>
              ) : (
                <Table aria-label={tr("Findings")} striped>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th>{tr("Severity")}</Table.Th>
                      <Table.Th>{tr("Bucket")}</Table.Th>
                      <Table.Th>{tr("Finding")}</Table.Th>
                      <Table.Th>{tr("Fix")}</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {rows.map(({ bucket, finding }) => {
                      const tab = openableTab(finding.fix_tab);
                      return (
                        <Table.Tr key={`${bucket}/${finding.id}`}>
                          <Table.Td>
                            <Badge color={COLORS[finding.severity]}>{SEVERITY_LABELS[finding.severity]}</Badge>
                          </Table.Td>
                          <Table.Td>{bucket}</Table.Td>
                          <Table.Td>
                            <Text size="sm" fw={500}>
                              {finding.title}
                            </Text>
                            <Text size="xs" c="dimmed">
                              {finding.explanation}
                            </Text>
                          </Table.Td>
                          <Table.Td>
                            {tab !== null ? (
                              <Button size="compact-sm" variant="light" onClick={() => fix(bucket, tab)}>
                                {tr("Open {label} tab", { label: FIX_TAB_LABELS[finding.fix_tab] })}
                              </Button>
                            ) : (
                              <Text size="xs" c="dimmed">
                                {tr("{label} (no editor in this version)", { label: FIX_TAB_LABELS[finding.fix_tab] })}
                              </Text>
                            )}
                          </Table.Td>
                        </Table.Tr>
                      );
                    })}
                  </Table.Tbody>
                </Table>
              )}
              {notes.length > 0 && (
                <Alert color="yellow" title={tr("Not everything could be checked")}>
                  <Stack gap={2}>
                    {notes.map((note) => (
                      <Text key={note} size="xs">
                        {note}
                      </Text>
                    ))}
                  </Stack>
                </Alert>
              )}
            </>
          )}
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              {tr("Close")}
            </Button>
          </Group>
        </Stack>
      )}
    </AppModal>
  );
}
