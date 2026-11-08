import { Stack, Table, Text } from "@mantine/core";
import type { ReactNode } from "react";
import type { ObjectEntry } from "../../api/types";
import { CopyField } from "../../components/CopyField";
import { tr } from "../../i18n";
import { formatBytes, formatCount, formatDateTime } from "../../lib/format";
import { baseName, displaySegment, s3Uri } from "../../lib/s3keys";
import type { TabContext } from "./apply";

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Table.Tr>
      <Table.Th scope="row" w={150} fw={500} style={{ verticalAlign: "top" }}>
        {label}
      </Table.Th>
      <Table.Td style={{ overflowWrap: "anywhere" }}>{children}</Table.Td>
    </Table.Tr>
  );
}

function Mono({ children }: { children: ReactNode }) {
  return (
    <Text size="sm" ff="monospace">
      {children}
    </Text>
  );
}

/** Read-only facts about one object, or a summary of the selection. */
export function GeneralTab({ ctx, entries }: { ctx: TabContext; entries: readonly ObjectEntry[] }) {
  const details = ctx.details;
  if (details === null) {
    const total = entries.reduce((sum, entry) => sum + entry.size, 0);
    const classes = new Map<string, number>();
    for (const entry of entries) {
      const name = entry.storage_class ?? tr("Unknown");
      classes.set(name, (classes.get(name) ?? 0) + 1);
    }
    return (
      <Stack gap="xs">
        <Text size="sm">
          {entries.length === 1
            ? tr("1 object in {bucket}, {size} in total.", { bucket: ctx.bucket, size: formatBytes(total) })
            : tr("{n} objects in {bucket}, {size} in total.", {
                n: formatCount(entries.length),
                bucket: ctx.bucket,
                size: formatBytes(total),
              })}
        </Text>
        <Text size="sm" c="dimmed">
          {tr("Storage classes: {classes}.", {
            classes: [...classes].map(([name, count]) => `${name} (${count})`).join(", "),
          })}
        </Text>
        <Text size="sm" c="dimmed">
          {tr("The other tabs change all of them at once and show the result for each object.")}
        </Text>
      </Stack>
    );
  }
  const checksums = Object.entries(details.checksums).filter((pair): pair is [string, string] => pair[1] !== null);
  return (
    <Stack gap="sm">
      <Table withTableBorder verticalSpacing={4} fz="sm">
        <Table.Tbody>
          <Row label={tr("Name")}>{displaySegment(baseName(details.key))}</Row>
          <Row label={tr("Type of object")}>{tr("Object")}</Row>
          <Row label={tr("Bucket")}>{ctx.bucket}</Row>
          <Row label={tr("Key")}>
            <Mono>{details.key}</Mono>
          </Row>
          <Row label={tr("Size")}>
            {formatBytes(details.size)} ({tr("{n} bytes", { n: formatCount(details.size) })})
          </Row>
          <Row label={tr("Last modified")}>
            {details.last_modified ? `${formatDateTime(details.last_modified)} (${details.last_modified})` : tr("Unknown")}
          </Row>
          <Row label={tr("Storage class")}>{details.storage_class ?? tr("Unknown")}</Row>
          <Row label="ETag">
            <Mono>{details.etag ?? tr("None")}</Mono>
          </Row>
          {details.version_id !== null ? (
            <Row label={tr("Version ID")}>
              <Mono>{details.version_id}</Mono>
            </Row>
          ) : null}
          {details.restore !== null ? (
            <Row label={tr("Restore")}>
              {details.restore.in_progress
                ? tr("A restore is in progress.")
                : details.restore.expires
                  ? tr("Restored copy available until {time}.", { time: formatDateTime(details.restore.expires) })
                  : tr("Restored copy available.")}
            </Row>
          ) : null}
          {checksums.length > 0 ? (
            <Row label={tr("Checksums")}>
              <Stack gap={2}>
                {checksums.map(([algorithm, value]) => (
                  <Mono key={algorithm}>
                    {algorithm.toUpperCase()}: {value}
                  </Mono>
                ))}
              </Stack>
            </Row>
          ) : null}
        </Table.Tbody>
      </Table>
      <CopyField label="S3 URI" value={s3Uri(ctx.bucket, details.key)} />
    </Stack>
  );
}
